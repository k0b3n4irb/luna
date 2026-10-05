//! Where a front-end keeps the user's files: the settings folder
//! (bindings, hotkeys, audio, last ROM folder) and the folders screenshots,
//! save states and input recordings are written to.
//!
//! The rules are pure functions of a [`PathEnv`], so every platform's
//! answer is checked by tests that run anywhere; [`config_file`] and
//! [`data_dir`] apply them to the real environment.

use std::path::PathBuf;

/// The two layouts luna knows. macOS takes the Unix one: its files have
/// always lived under `~/.config/luna` and `~/.local/luna`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    /// Linux, macOS and the other Unixes.
    Unix,
    /// Windows.
    Windows,
}

/// What the folder rules read from the environment. A variable that is
/// unset or empty is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEnv {
    /// Which layout applies.
    pub family: OsFamily,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<PathBuf>,
    /// `%APPDATA%` (the roaming application-data folder).
    pub appdata: Option<PathBuf>,
    /// `%USERPROFILE%`.
    pub userprofile: Option<PathBuf>,
}

impl PathEnv {
    /// The environment of this process. Where a variable is missing the
    /// operating system's own answer stands in for it: the account's home
    /// folder for `$HOME` / `%USERPROFILE%`, the known folder for
    /// `%APPDATA%`.
    #[must_use]
    pub fn from_process() -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let windows = cfg!(windows);
        Self {
            family: if windows {
                OsFamily::Windows
            } else {
                OsFamily::Unix
            },
            home: var("HOME").or_else(|| (!windows).then(dirs::home_dir).flatten()),
            // Read as text, as it always was: a value that is not UTF-8 is
            // skipped and `$HOME/.config` is used.
            xdg_config_home: std::env::var("XDG_CONFIG_HOME")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            appdata: var("APPDATA").or_else(|| windows.then(dirs::config_dir).flatten()),
            userprofile: var("USERPROFILE").or_else(|| windows.then(dirs::home_dir).flatten()),
        }
    }

    /// The folder luna's files sit in on Windows: `%APPDATA%\luna`, or the
    /// same folder spelled from `%USERPROFILE%`.
    fn windows_root(&self) -> Option<PathBuf> {
        self.appdata
            .clone()
            .or_else(|| {
                self.userprofile
                    .as_ref()
                    .map(|p| p.join("AppData").join("Roaming"))
            })
            .map(|d| d.join("luna"))
    }

    /// The settings folder: `$XDG_CONFIG_HOME/luna`, else
    /// `$HOME/.config/luna`; `%APPDATA%\luna` on Windows. `None` when the
    /// environment names no folder at all.
    #[must_use]
    pub fn config_dir(&self) -> Option<PathBuf> {
        match self.family {
            OsFamily::Unix => self
                .xdg_config_home
                .clone()
                .or_else(|| self.home.as_ref().map(|h| h.join(".config")))
                .map(|d| d.join("luna")),
            OsFamily::Windows => self.windows_root(),
        }
    }

    /// The settings file `file` (`input.json`, `last_rom_dir`, …) inside
    /// [`Self::config_dir`].
    #[must_use]
    pub fn config_file(&self, file: &str) -> Option<PathBuf> {
        self.config_dir().map(|d| d.join(file))
    }

    /// The folder for one kind of file the user produces (`screenshots`,
    /// `states`, `recordings`): `$HOME/.local/luna/<kind>`, or
    /// `%APPDATA%\luna\<kind>` on Windows. With no folder to anchor it, the
    /// bare relative `<kind>`, which lands in the working directory.
    #[must_use]
    pub fn data_dir(&self, kind: &str) -> PathBuf {
        let root = match self.family {
            OsFamily::Unix => self.home.as_ref().map(|h| h.join(".local").join("luna")),
            OsFamily::Windows => self.windows_root(),
        };
        root.map_or_else(|| PathBuf::from(kind), |r| r.join(kind))
    }
}

/// [`PathEnv::config_file`] for this process.
#[must_use]
pub fn config_file(file: &str) -> Option<PathBuf> {
    PathEnv::from_process().config_file(file)
}

/// [`PathEnv::data_dir`] for this process.
#[must_use]
pub fn data_dir(kind: &str) -> PathBuf {
    PathEnv::from_process().data_dir(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG_FILES: [&str; 4] = ["input.json", "hotkeys.json", "audio.json", "last_rom_dir"];
    const DATA_KINDS: [&str; 3] = ["screenshots", "states", "recordings"];

    fn unix(home: Option<&str>, xdg: Option<&str>) -> PathEnv {
        PathEnv {
            family: OsFamily::Unix,
            home: home.map(PathBuf::from),
            xdg_config_home: xdg.map(PathBuf::from),
            appdata: None,
            userprofile: None,
        }
    }

    fn windows(appdata: Option<&str>, userprofile: Option<&str>) -> PathEnv {
        PathEnv {
            family: OsFamily::Windows,
            home: None,
            xdg_config_home: None,
            appdata: appdata.map(PathBuf::from),
            userprofile: userprofile.map(PathBuf::from),
        }
    }

    /// `base` followed by `parts`, joined the way the host joins paths (a
    /// Windows root stays one opaque component when tested on Unix).
    fn under(base: &str, parts: &[&str]) -> PathBuf {
        parts
            .iter()
            .fold(PathBuf::from(base), |path, part| path.join(part))
    }

    // Linux and macOS: the locations users already have files in. These
    // must never move.

    #[test]
    fn unix_settings_follow_xdg_config_home_when_it_is_set() {
        for home in [Some("/home/k"), Some("/Users/k"), None] {
            let env = unix(home, Some("/xdg"));
            for file in CONFIG_FILES {
                assert_eq!(env.config_file(file), Some(under("/xdg/luna", &[file])));
            }
        }
    }

    #[test]
    fn unix_settings_fall_back_to_home_dot_config() {
        for home in ["/home/k", "/Users/k"] {
            let env = unix(Some(home), None);
            for file in CONFIG_FILES {
                assert_eq!(
                    env.config_file(file),
                    Some(under(home, &[".config", "luna", file]))
                );
            }
        }
    }

    #[test]
    fn unix_data_folders_sit_under_home_dot_local_whatever_xdg_says() {
        for home in ["/home/k", "/Users/k"] {
            for xdg in [Some("/xdg"), None] {
                let env = unix(Some(home), xdg);
                for kind in DATA_KINDS {
                    assert_eq!(env.data_dir(kind), under(home, &[".local", "luna", kind]));
                }
            }
        }
    }

    #[test]
    fn unix_without_home_has_no_settings_folder_and_relative_data_folders() {
        let env = unix(None, None);
        for file in CONFIG_FILES {
            assert_eq!(env.config_file(file), None);
        }
        // `$XDG_CONFIG_HOME` names the settings folder only.
        for env in [env, unix(None, Some("/xdg"))] {
            for kind in DATA_KINDS {
                assert_eq!(env.data_dir(kind), PathBuf::from(kind));
            }
        }
    }

    // Windows: everything under one `luna` folder in `%APPDATA%`, next to
    // the firmware folder `Emulator::firmware_dir` already puts there.

    const APPDATA: &str = r"C:\Users\k\AppData\Roaming";

    #[test]
    fn windows_keeps_settings_and_data_under_appdata_luna() {
        let env = windows(Some(APPDATA), Some(r"C:\Users\k"));
        for file in CONFIG_FILES {
            assert_eq!(env.config_file(file), Some(under(APPDATA, &["luna", file])));
        }
        for kind in DATA_KINDS {
            assert_eq!(env.data_dir(kind), under(APPDATA, &["luna", kind]));
        }
    }

    #[test]
    fn windows_without_appdata_spells_it_from_userprofile() {
        let env = windows(None, Some(r"C:\Users\k"));
        let roaming = under(r"C:\Users\k", &["AppData", "Roaming", "luna"]);
        assert_eq!(
            env.config_file("input.json"),
            Some(roaming.join("input.json"))
        );
        assert_eq!(env.data_dir("states"), roaming.join("states"));
    }

    #[test]
    fn windows_ignores_the_unix_variables() {
        // A shell such as Git Bash sets `HOME`; the folder must not depend
        // on which shell started luna.
        let mut env = windows(Some(APPDATA), None);
        env.home = Some(PathBuf::from("/c/Users/k"));
        env.xdg_config_home = Some(PathBuf::from("/xdg"));
        assert_eq!(
            env.config_file("input.json"),
            Some(under(APPDATA, &["luna", "input.json"]))
        );
        assert_eq!(
            env.data_dir("screenshots"),
            under(APPDATA, &["luna", "screenshots"])
        );
    }

    #[test]
    fn windows_with_nothing_set_degrades_like_unix_without_home() {
        let env = windows(None, None);
        assert_eq!(env.config_file("input.json"), None);
        assert_eq!(env.data_dir("recordings"), PathBuf::from("recordings"));
    }

    /// On the machine running the tests, the resolver names the folders
    /// the GUI wrote to before it existed: `$XDG_CONFIG_HOME/luna` or
    /// `$HOME/.config/luna`, and `$HOME/.local/luna/<kind>`. The proof
    /// that Linux and macOS users keep their files.
    #[cfg(unix)]
    #[test]
    fn this_process_resolves_the_folders_the_gui_always_used() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        let env = PathEnv::from_process();
        let settings = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(".config"), PathBuf::from)
            .join("luna");
        for file in CONFIG_FILES {
            assert_eq!(env.config_file(file), Some(settings.join(file)));
            assert_eq!(config_file(file), Some(settings.join(file)));
        }
        for kind in DATA_KINDS {
            assert_eq!(
                env.data_dir(kind),
                home.join(".local").join("luna").join(kind)
            );
            assert_eq!(data_dir(kind), home.join(".local").join("luna").join(kind));
        }
    }

    #[test]
    fn unix_ignores_the_windows_variables() {
        let mut env = unix(Some("/home/k"), None);
        env.appdata = Some(PathBuf::from(APPDATA));
        env.userprofile = Some(PathBuf::from(r"C:\Users\k"));
        assert_eq!(
            env.config_file("input.json"),
            Some(PathBuf::from("/home/k/.config/luna/input.json"))
        );
        assert_eq!(
            env.data_dir("states"),
            PathBuf::from("/home/k/.local/luna/states")
        );
    }
}
