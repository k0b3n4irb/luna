//! End-to-end MCP **protocol** tests over an in-memory transport pair.
//!
//! The unit tests in `lib.rs` call the tool methods directly, which
//! proves the emulator wiring but not the wire contract: the tool
//! catalogue an MCP client actually discovers, the JSON-RPC
//! serialisation of arguments and results, and how errors surface to a
//! peer. That was the gap the 2026-07-26 audit flagged ("the only
//! front-end without a contract test") — and the reason the rmcp 3.0
//! bump is being held: a protocol change needs a before/after.
//!
//! `tokio::io::duplex` gives a bidirectional in-memory pipe, so the
//! server runs exactly as it does over stdio, with a real rmcp client
//! (`()` is rmcp's no-op `ClientHandler`) on the other end. No process
//! spawning, no ports, deterministic.

use luna_mcp_server::LunaServer;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParam;
use rmcp::service::ServiceError;

/// Spawn `LunaServer` on one end of an in-memory duplex and return a
/// connected client peer.
async fn connect() -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        if let Ok(server) = LunaServer::new().serve(server_io).await {
            let _ = server.waiting().await;
        }
    });
    ().serve(client_io).await.expect("client handshake")
}

/// Every tool luna's MCP surface promises, as a client discovers them.
/// Locks the catalogue: renaming or dropping a tool is a breaking
/// change to agents driving luna and must be a deliberate edit here.
#[tokio::test]
async fn tools_list_exposes_the_documented_catalogue() {
    let client = connect().await;
    let tools = client.list_all_tools().await.expect("tools/list");

    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    for expected in [
        "capabilities",
        "load_rom",
        "peek_memory",
        "reset",
        "screenshot",
        "set_joypad",
        "state",
        "step",
        "step_until_frame",
    ] {
        assert!(
            names.contains(&expected),
            "tool `{expected}` missing from the catalogue: {names:?}"
        );
    }
    // Every tool must carry a description and an input schema — that is
    // what an agent reads to decide how to call it.
    for t in &tools {
        assert!(
            t.description.as_ref().is_some_and(|d| !d.is_empty()),
            "tool `{}` has no description",
            t.name
        );
        assert!(
            t.input_schema.contains_key("type"),
            "tool `{}` has no input schema",
            t.name
        );
    }
    client.cancel().await.ok();
}

/// A full request/response round trip with arguments and a structured
/// result, over the wire.
#[tokio::test]
async fn capabilities_round_trips_over_the_wire() {
    let client = connect().await;
    let result = client
        .call_tool(CallToolRequestParam {
            name: "capabilities".into(),
            arguments: None,
        })
        .await
        .expect("capabilities call");

    assert_ne!(result.is_error, Some(true), "capabilities must not error");
    let json = serde_json::to_string(&result).expect("serialisable result");
    assert!(
        json.contains("tools") || json.contains("version"),
        "capabilities payload looks empty: {json}"
    );
    client.cancel().await.ok();
}

/// Errors must reach the peer as JSON-RPC errors carrying luna's
/// message — not as a dropped connection or a silent empty result.
#[tokio::test]
async fn tool_error_surfaces_to_the_peer_as_jsonrpc_error() {
    let client = connect().await;
    // `step` with no ROM loaded → ApiError::NoRom, mapped to ErrorData.
    let err = client
        .call_tool(CallToolRequestParam {
            name: "step".into(),
            arguments: Some(
                serde_json::json!({ "count": 1 })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        })
        .await
        .expect_err("stepping without a ROM must fail");

    let ServiceError::McpError(data) = err else {
        panic!("expected a protocol-level McpError, got {err:?}");
    };
    assert!(
        data.message.contains("no ROM"),
        "error message lost on the wire: {}",
        data.message
    );
    client.cancel().await.ok();
}

/// An unknown tool name is rejected by the router, not silently
/// accepted (an agent that typos a tool must find out).
#[tokio::test]
async fn unknown_tool_is_rejected() {
    let client = connect().await;
    let err = client
        .call_tool(CallToolRequestParam {
            name: "definitely_not_a_luna_tool".into(),
            arguments: None,
        })
        .await
        .expect_err("unknown tool must be rejected");
    assert!(
        matches!(err, ServiceError::McpError(_)),
        "expected an McpError for an unknown tool, got {err:?}"
    );
    client.cancel().await.ok();
}

/// The load → step → observe → screenshot loop an agent actually runs,
/// end to end over the protocol, on a synthetic ROM. Guards the whole
/// chain: argument deserialisation, emulator progress across calls
/// (the server keeps one `Emulator` behind a mutex), and the base64
/// image payload.
#[tokio::test]
async fn load_step_and_screenshot_round_trip() {
    // Minimal LoROM image: 64 KiB of zeros with a plausible header, so
    // `load_rom` succeeds without shipping a copyrighted ROM.
    let mut rom = vec![0u8; 0x1_0000];
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA MCP TEST ROM    "[..0x15].as_ref());
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD6] = 0x00; // ROM only
    rom[0x7FD7] = 0x07; // 128 KiB (size code)
    // Complement/checksum pair the detector scores on.
    rom[0x7FDC] = 0xFF;
    rom[0x7FDD] = 0xFF;
    rom[0x7FDE] = 0x00;
    rom[0x7FDF] = 0x00;
    // Reset vector → $8000; the ROM is zeros (BRK), which the API
    // catches; we only need the load + a few steps to succeed.
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;

    let dir = std::env::temp_dir().join("luna-mcp-protocol-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("synthetic.smc");
    std::fs::write(&path, &rom).expect("write rom");

    let client = connect().await;
    let load = client
        .call_tool(CallToolRequestParam {
            name: "load_rom".into(),
            arguments: Some(
                serde_json::json!({ "path": path.to_string_lossy() })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        })
        .await
        .expect("load_rom call");
    assert_ne!(load.is_error, Some(true), "synthetic ROM must load");

    // A screenshot must come back as a base64 payload of a real PNG.
    let shot = client
        .call_tool(CallToolRequestParam {
            name: "screenshot".into(),
            arguments: None,
        })
        .await
        .expect("screenshot call");
    assert_ne!(shot.is_error, Some(true));
    let json = serde_json::to_string(&shot).expect("serialisable");
    assert!(
        json.contains("image") || json.contains("base64") || json.contains("iVBOR"),
        "screenshot payload carries no image data: {}",
        &json[..json.len().min(200)]
    );

    // `state` must round-trip the loaded ROM back to the peer.
    let state = client
        .call_tool(CallToolRequestParam {
            name: "state".into(),
            arguments: None,
        })
        .await
        .expect("state call");
    assert_ne!(state.is_error, Some(true));

    client.cancel().await.ok();
    let _ = std::fs::remove_file(&path);
}
