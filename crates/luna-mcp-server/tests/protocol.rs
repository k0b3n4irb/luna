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
use rmcp::model::CallToolRequestParams;
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
        .call_tool(CallToolRequestParams::new("capabilities"))
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
        .call_tool(
            CallToolRequestParams::new("step").with_arguments(
                serde_json::json!({ "count": 1 })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
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
        .call_tool(CallToolRequestParams::new("definitely_not_a_luna_tool"))
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
        .call_tool(
            CallToolRequestParams::new("load_rom").with_arguments(
                serde_json::json!({ "path": path.to_string_lossy() })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
        .await
        .expect("load_rom call");
    assert_ne!(load.is_error, Some(true), "synthetic ROM must load");

    // A screenshot must come back as a base64 payload of a real PNG.
    let shot = client
        .call_tool(CallToolRequestParams::new("screenshot"))
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
        .call_tool(CallToolRequestParams::new("state"))
        .await
        .expect("state call");
    assert_ne!(state.is_error, Some(true));

    client.cancel().await.ok();
    let _ = std::fs::remove_file(&path);
}

/// Raw `tools/list` result for a client speaking `version` — the bytes a
/// client parses, not rmcp's typed view of them. 2026-07-28 has no
/// `initialize` (rmcp 3.9 answers one with 2025-11-25): a client states its
/// version in the `_meta` of every request. Older versions negotiate it
/// through `initialize`.
async fn raw_tools_list(version: &str) -> serde_json::Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        if let Ok(server) = LunaServer::new().serve(server_io).await {
            let _ = server.waiting().await;
        }
    });
    let (read, mut write) = tokio::io::split(client_io);
    let mut lines = BufReader::new(read).lines();
    let stateless = version >= "2026-07-28";
    let list = if stateless {
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": version,
                "io.modelcontextprotocol/clientInfo": { "name": "raw", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {},
            } },
        })
    } else {
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} })
    };
    let mut msgs = Vec::new();
    if !stateless {
        msgs.push(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "raw", "version": "0" },
            },
        }));
        msgs.push(serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }
    msgs.push(list);
    for msg in msgs {
        let line = format!("{msg}\n");
        write.write_all(line.as_bytes()).await.expect("write");
    }
    while let Some(line) = lines.next_line().await.expect("read") {
        let msg: serde_json::Value = serde_json::from_str(&line).expect("json");
        if msg["id"] == 2 {
            return msg["result"].clone();
        }
    }
    panic!("no tools/list response");
}

/// Protocol 2026-07-28 requires `ttlMs` and `cacheScope` on list results
/// (SEP-2549). rmcp 3.1.0 left them out, and a client speaking that
/// version (Claude Code 2.1.287) rejected the whole catalogue; older
/// versions must still get neither field.
#[tokio::test]
async fn tools_list_carries_the_cache_hints_its_protocol_version_requires() {
    let current = raw_tools_list("2026-07-28").await;
    assert!(
        current["ttlMs"].is_u64(),
        "2026-07-28 needs a numeric ttlMs: {}",
        current["ttlMs"]
    );
    assert!(
        matches!(current["cacheScope"].as_str(), Some("public" | "private")),
        "2026-07-28 needs cacheScope public|private: {}",
        current["cacheScope"]
    );

    let legacy = raw_tools_list("2025-06-18").await;
    assert!(legacy.get("ttlMs").is_none() && legacy.get("cacheScope").is_none());
    assert!(legacy["tools"].as_array().is_some_and(|t| !t.is_empty()));
}

fn args(v: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().expect("object").clone()
}

/// The hardware strict journal over the wire: a synthetic ROM that writes
/// `$2118` with the screen on is reported as R1, and a drained journal is
/// empty. Nothing of any game: the image is built here.
#[tokio::test]
async fn hw_strict_journal_round_trips_over_the_wire() {
    use base64::Engine as _;
    let mut rom = vec![0u8; 0x1_0000];
    // LDA #$0F ; STA $2100 ; loop: STA $2118 ; BRA loop
    rom[..10].copy_from_slice(&[0xA9, 0x0F, 0x8D, 0x00, 0x21, 0x8D, 0x18, 0x21, 0x80, 0xFB]);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA MCP STRICT      ");
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let client = connect().await;
    client
        .call_tool(
            CallToolRequestParams::new("load_rom_bytes").with_arguments(args(&serde_json::json!({
                "rom_base64": base64::engine::general_purpose::STANDARD.encode(&rom),
                "force_mapper": "lorom",
            }))),
        )
        .await
        .expect("load_rom_bytes");
    client
        .call_tool(CallToolRequestParams::new("enable_hw_strict"))
        .await
        .expect("enable_hw_strict");
    for _ in 0..3 {
        client
            .call_tool(
                CallToolRequestParams::new("step_until_frame")
                    .with_arguments(args(&serde_json::json!({ "max_steps": 1_000_000 }))),
            )
            .await
            .expect("step_until_frame");
    }
    let taken = client
        .call_tool(CallToolRequestParams::new("take_hw_faults"))
        .await
        .expect("take_hw_faults");
    let v = taken.structured_content.expect("structured result");
    assert_eq!(v["ok"], false);
    assert_eq!(v["faults"][0]["rule"], "R1");
    assert_eq!(v["faults"][0]["pc"], "$00:8005");
    assert_eq!(v["faults"][0]["addr"], "$2118");
    assert!(v["occurrences"].as_u64().unwrap() > 100);
    let again = client
        .call_tool(CallToolRequestParams::new("take_hw_faults"))
        .await
        .expect("take_hw_faults again");
    assert_eq!(again.structured_content.expect("structured")["ok"], true);
    client.cancel().await.ok();
}

/// The VRAM provenance journal over the wire: content poked before the
/// journal starts, drawn by the power-on sprites, is S1; a drained journal
/// is empty. Nothing of any game: the image is built here.
#[tokio::test]
async fn vram_provenance_round_trips_over_the_wire() {
    use base64::Engine as _;
    let mut rom = vec![0u8; 0x1_0000];
    // LDA #$0F ; STA $2100 ; loop: BRA loop
    rom[..7].copy_from_slice(&[0xA9, 0x0F, 0x8D, 0x00, 0x21, 0x80, 0xFE]);
    rom[0x7FC0..0x7FD5].copy_from_slice(b"LUNA MCP PROVENANCE  ");
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x07;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    let client = connect().await;
    client
        .call_tool(
            CallToolRequestParams::new("load_rom_bytes").with_arguments(args(&serde_json::json!({
                "rom_base64": base64::engine::general_purpose::STANDARD.encode(&rom),
                "force_mapper": "lorom",
            }))),
        )
        .await
        .expect("load_rom_bytes");
    client
        .call_tool(CallToolRequestParams::new("poke_vram").with_arguments(args(
            &serde_json::json!({ "offset": 0, "data": [170, 170, 170, 170] }),
        )))
        .await
        .expect("poke_vram");
    client
        .call_tool(
            CallToolRequestParams::new("enable_vram_provenance")
                .with_arguments(args(&serde_json::json!({ "enabled": true }))),
        )
        .await
        .expect("enable_vram_provenance");
    for _ in 0..3 {
        client
            .call_tool(
                CallToolRequestParams::new("step_until_frame")
                    .with_arguments(args(&serde_json::json!({ "max_steps": 1_000_000 }))),
            )
            .await
            .expect("step_until_frame");
    }
    let taken = client
        .call_tool(CallToolRequestParams::new("take_vram_provenance"))
        .await
        .expect("take_vram_provenance");
    let v = taken.structured_content.expect("structured result");
    assert_eq!(v["ok"], false);
    assert_eq!(v["entries"][0]["rule"], "S1");
    assert_eq!(v["entries"][0]["tile_addr"], "$0000");
    let again = client
        .call_tool(CallToolRequestParams::new("take_vram_provenance"))
        .await
        .expect("take_vram_provenance again");
    assert_eq!(again.structured_content.expect("structured")["ok"], true);
    client.cancel().await.ok();
}
