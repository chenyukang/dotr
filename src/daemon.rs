#[cfg(unix)]
mod imp {
    use std::{
        fs::{self, OpenOptions},
        io,
        os::unix::process::CommandExt,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail};
    use serde::{Deserialize, Serialize};

    use crate::{
        config::{Config, config_path},
        environment::Environment,
        paths::absolutize,
        structured_log,
    };

    pub const DAEMON_NAME: &str = "dotr-watch";

    const CONFIG_FILE_NAME: &str = "daemon.toml";
    const PID_FILE_NAME: &str = "dotr-watch.pid";
    const LOG_FILE_NAME: &str = "dotr-watch.jsonl";
    const DEFAULT_LOG_LEVEL: &str = "info";
    const STOP_TIMEOUT: Duration = Duration::from_secs(5);
    const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DaemonPaths {
        pub config: PathBuf,
        pub pid_file: PathBuf,
        pub log_path: PathBuf,
        pub log_level: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StartReport {
        pub name: &'static str,
        pub pid: u32,
        pub already_running: bool,
        pub log_path: PathBuf,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StopReport {
        pub name: &'static str,
        pub pid: Option<u32>,
        pub stopped: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DaemonState {
        NotConfigured,
        Running,
        Stopped,
        StalePid,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StatusReport {
        pub name: &'static str,
        pub config: PathBuf,
        pub configured: bool,
        pub state: DaemonState,
        pub pid: Option<u32>,
        pub repo_root: Option<PathBuf>,
        pub log_path: PathBuf,
        pub log_level: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct InstalledDaemonConfig {
        version: u32,
        name: String,
        executable: String,
        repo_root: String,
        pid_file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_path: Option<String>,
        #[serde(default = "default_installed_log_level")]
        log_level: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_log: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_log: Option<String>,
    }

    impl InstalledDaemonConfig {
        fn from_paths(executable: &Path, repo_root: &Path, paths: &DaemonPaths) -> Self {
            Self {
                version: 1,
                name: DAEMON_NAME.to_string(),
                executable: executable.to_string_lossy().into_owned(),
                repo_root: repo_root.to_string_lossy().into_owned(),
                pid_file: paths.pid_file.to_string_lossy().into_owned(),
                log_path: Some(paths.log_path.to_string_lossy().into_owned()),
                log_level: paths.log_level.clone(),
                log_file: None,
                stdout_log: None,
                stderr_log: None,
            }
        }

        fn executable(&self) -> PathBuf {
            PathBuf::from(&self.executable)
        }

        fn repo_root(&self) -> PathBuf {
            PathBuf::from(&self.repo_root)
        }

        fn pid_file(&self) -> PathBuf {
            PathBuf::from(&self.pid_file)
        }

        fn log_path(&self) -> PathBuf {
            self.log_path
                .as_deref()
                .or(self.log_file.as_deref())
                .or(self.stderr_log.as_deref())
                .or(self.stdout_log.as_deref())
                .map(PathBuf::from)
                .unwrap_or_else(|| self.pid_file().with_extension("jsonl"))
        }

        fn log_level(&self) -> &str {
            &self.log_level
        }
    }

    pub fn start(env: &Environment, repo_root: Option<&Path>) -> Result<StartReport> {
        let existing_config = load_config_if_exists(env)?;
        if let Some(config) = existing_config.as_ref() {
            let pid_file = config.pid_file();
            if let Some(pid) = read_pid_file(&pid_file)? {
                if process_exists(pid) {
                    return Ok(StartReport {
                        name: DAEMON_NAME,
                        pid,
                        already_running: true,
                        log_path: config.log_path(),
                    });
                }
                remove_file_if_exists(&pid_file)?;
            }
        }

        let config = match repo_root {
            Some(repo_root) => configure(env, repo_root)?,
            None => existing_config.with_context(|| {
                "daemon is not configured; run `dotr daemon start` from a dotr repo or pass --repo"
            })?,
        };

        spawn_from_config(&config)
    }

    fn configure(env: &Environment, repo_root: &Path) -> Result<InstalledDaemonConfig> {
        if !config_path(repo_root).is_file() {
            bail!("{} is not a dotr repository", repo_root.display());
        }

        let dotr_config = Config::load(repo_root)?;
        let repo_root = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());
        let paths = daemon_paths_for_repo(env, &repo_root, &dotr_config);
        create_parent_dir(&paths.config)?;
        create_parent_dir(&paths.pid_file)?;
        create_parent_dir(&paths.log_path)?;

        let executable = std::env::current_exe().context("failed to resolve current executable")?;
        let config = InstalledDaemonConfig::from_paths(&executable, &repo_root, &paths);
        let toml = toml::to_string_pretty(&config).context("failed to serialize daemon config")?;
        fs::write(&paths.config, toml)
            .with_context(|| format!("failed to write {}", paths.config.display()))?;

        Ok(config)
    }

    fn spawn_from_config(config: &InstalledDaemonConfig) -> Result<StartReport> {
        let pid_file = config.pid_file();
        create_parent_dir(&pid_file)?;
        create_parent_dir(&config.log_path())?;

        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.log_path())
            .with_context(|| format!("failed to open {}", config.log_path().display()))?;
        let stdout = log
            .try_clone()
            .with_context(|| format!("failed to clone {}", config.log_path().display()))?;

        let repo_root = config.repo_root();
        let mut command = Command::new(config.executable());
        command
            .arg("--repo")
            .arg(&repo_root)
            .arg("watch")
            .current_dir(&repo_root)
            .env("DOTR_LOG_LEVEL", config.log_level())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log));

        // SAFETY: pre_exec runs in the child process after fork and before exec.
        // Calling setsid here detaches the watcher from the invoking terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }

        let child = command
            .spawn()
            .with_context(|| format!("failed to start daemon for {}", repo_root.display()))?;
        let pid = child.id();
        fs::write(&pid_file, format!("{pid}\n"))
            .with_context(|| format!("failed to write {}", pid_file.display()))?;

        Ok(StartReport {
            name: DAEMON_NAME,
            pid,
            already_running: false,
            log_path: config.log_path(),
        })
    }

    pub fn stop(env: &Environment) -> Result<StopReport> {
        let Some(config) = load_config_if_exists(env)? else {
            return Ok(StopReport {
                name: DAEMON_NAME,
                pid: None,
                stopped: false,
            });
        };
        let pid_file = config.pid_file();
        let Some(pid) = read_pid_file(&pid_file)? else {
            return Ok(StopReport {
                name: DAEMON_NAME,
                pid: None,
                stopped: false,
            });
        };

        if !process_exists(pid) {
            remove_file_if_exists(&pid_file)?;
            return Ok(StopReport {
                name: DAEMON_NAME,
                pid: Some(pid),
                stopped: false,
            });
        }

        send_signal(pid, libc::SIGTERM)?;
        wait_until_stopped(pid, STOP_TIMEOUT)?;
        remove_file_if_exists(&pid_file)?;

        Ok(StopReport {
            name: DAEMON_NAME,
            pid: Some(pid),
            stopped: true,
        })
    }

    pub fn status(env: &Environment) -> Result<StatusReport> {
        let paths = daemon_paths(env);
        if !paths.config.is_file() {
            return Ok(StatusReport {
                name: DAEMON_NAME,
                config: paths.config,
                configured: false,
                state: DaemonState::NotConfigured,
                pid: None,
                repo_root: None,
                log_path: paths.log_path,
                log_level: paths.log_level,
            });
        }

        let config = read_config_file(&paths.config)?;
        let pid = read_pid_file(&config.pid_file())?;
        let state = match pid {
            Some(pid) if process_exists(pid) => DaemonState::Running,
            Some(_) => DaemonState::StalePid,
            None => DaemonState::Stopped,
        };

        Ok(StatusReport {
            name: DAEMON_NAME,
            config: paths.config,
            configured: true,
            state,
            pid,
            repo_root: Some(config.repo_root()),
            log_path: config.log_path(),
            log_level: config.log_level().to_string(),
        })
    }

    pub fn daemon_paths(env: &Environment) -> DaemonPaths {
        default_daemon_paths(env)
    }

    pub fn is_configured(env: &Environment) -> bool {
        daemon_paths(env).config.is_file()
    }

    fn daemon_paths_for_repo(env: &Environment, repo_root: &Path, config: &Config) -> DaemonPaths {
        let defaults = default_daemon_paths(env);
        DaemonPaths {
            config: defaults.config,
            pid_file: defaults.pid_file,
            log_path: config
                .daemon
                .log_path
                .as_deref()
                .or(config.daemon.log_file.as_deref())
                .or(config.daemon.stderr_log.as_deref())
                .or(config.daemon.stdout_log.as_deref())
                .map(|path| resolve_config_path(env, repo_root, path))
                .unwrap_or(defaults.log_path),
            log_level: configured_log_level(config),
        }
    }

    fn default_daemon_paths(env: &Environment) -> DaemonPaths {
        let config_dir = env.home().join(".config").join("dotr");
        let state_dir = env.home().join(".local").join("state").join("dotr");

        DaemonPaths {
            config: config_dir.join(CONFIG_FILE_NAME),
            pid_file: state_dir.join(PID_FILE_NAME),
            log_path: state_dir.join(LOG_FILE_NAME),
            log_level: DEFAULT_LOG_LEVEL.to_string(),
        }
    }

    fn configured_log_level(config: &Config) -> String {
        let raw = config
            .daemon
            .log_level
            .as_deref()
            .unwrap_or(DEFAULT_LOG_LEVEL);
        structured_log::normalize_level(raw).unwrap_or(DEFAULT_LOG_LEVEL).to_string()
    }

    fn default_installed_log_level() -> String {
        DEFAULT_LOG_LEVEL.to_string()
    }

    fn resolve_config_path(env: &Environment, repo_root: &Path, raw: &str) -> PathBuf {
        absolutize(&env.expand_tilde(raw), repo_root)
    }

    fn load_config_if_exists(env: &Environment) -> Result<Option<InstalledDaemonConfig>> {
        let path = daemon_paths(env).config;
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(read_config_file(&path)?))
    }

    fn read_config_file(path: &Path) -> Result<InstalledDaemonConfig> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    }

    fn read_pid_file(path: &Path) -> Result<Option<u32>> {
        if !path.is_file() {
            return Ok(None);
        }

        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Some(parse_pid(raw.trim())?))
    }

    fn parse_pid(raw: &str) -> Result<u32> {
        let pid = raw
            .parse::<u32>()
            .with_context(|| format!("invalid daemon pid: {raw}"))?;
        if pid == 0 {
            bail!("invalid daemon pid: 0");
        }
        Ok(pid)
    }

    fn process_exists(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: kill with signal 0 does not send a signal; it only asks the OS
        // whether the pid exists and is signalable by this process.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn send_signal(pid: u32, signal: libc::c_int) -> Result<()> {
        let pid = libc::pid_t::try_from(pid).context("daemon pid does not fit pid_t")?;
        // SAFETY: sending SIGTERM to the pid stored by dotr is the requested stop
        // operation. The return value is checked and converted to an io error.
        let rc = unsafe { libc::kill(pid, signal) };
        if rc == -1 {
            return Err(io::Error::last_os_error()).context("failed to signal daemon");
        }
        Ok(())
    }

    fn wait_until_stopped(pid: u32, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !process_exists(pid) {
                return Ok(());
            }
            thread::sleep(STOP_POLL_INTERVAL);
        }

        bail!(
            "daemon pid {pid} did not stop within {}s",
            timeout.as_secs()
        )
    }

    fn create_parent_dir(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(())
    }

    fn remove_file_if_exists(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::tempdir;

        fn env_for(home: &Path) -> Environment {
            Environment::new(home.to_path_buf()).unwrap()
        }

        #[test]
        fn daemon_paths_use_user_config_and_state_dirs() {
            let env = env_for(Path::new("/Users/me"));
            let paths = daemon_paths(&env);

            assert_eq!(
                paths.config,
                PathBuf::from("/Users/me/.config/dotr/daemon.toml")
            );
            assert_eq!(
                paths.pid_file,
                PathBuf::from("/Users/me/.local/state/dotr/dotr-watch.pid")
            );
            assert_eq!(
                paths.log_path,
                PathBuf::from("/Users/me/.local/state/dotr/dotr-watch.jsonl")
            );
            assert_eq!(paths.log_level, "info");
        }

        #[test]
        fn configure_writes_generic_daemon_config() {
            let home = tempdir().unwrap();
            let repo = tempdir().unwrap();
            fs::write(repo.path().join("dotr.toml"), "").unwrap();
            let env = env_for(home.path());

            let config = configure(&env, repo.path()).unwrap();
            let paths = daemon_paths(&env);
            let raw = fs::read_to_string(&paths.config).unwrap();

            assert_eq!(config.name, DAEMON_NAME);
            assert!(raw.contains("name = \"dotr-watch\""));
            assert!(raw.contains("version = 1"));
            assert!(raw.contains("dotr-watch.pid"));
            assert!(!raw.contains("LaunchAgents"));
            assert!(!raw.contains(".plist"));
        }

        #[test]
        fn configure_uses_configured_log_path_and_level() {
            let home = tempdir().unwrap();
            let repo = tempdir().unwrap();
            fs::write(
                repo.path().join("dotr.toml"),
                r#"
                [daemon]
                log_path = "~/logs/dotr-watch.jsonl"
                log_level = "debug"
                "#,
            )
            .unwrap();
            let env = env_for(home.path());
            let repo_root = repo.path().canonicalize().unwrap();

            let config = configure(&env, repo.path()).unwrap();

            assert_eq!(config.log_path(), home.path().join("logs/dotr-watch.jsonl"));
            assert_eq!(config.log_level(), "debug");

            let raw = fs::read_to_string(daemon_paths(&env).config).unwrap();
            assert!(raw.contains(&format!(
                "log_path = \"{}\"",
                home.path().join("logs/dotr-watch.jsonl").display()
            )));
            assert!(raw.contains("log_level = \"debug\""));
            assert!(!raw.contains("stdout_log"));
            assert!(!raw.contains("stderr_log"));
        }

        #[test]
        fn configure_accepts_legacy_log_paths() {
            let home = tempdir().unwrap();
            let repo = tempdir().unwrap();
            fs::write(
                repo.path().join("dotr.toml"),
                r#"
                [daemon]
                stderr_log = "logs/dotr.err.log"
                "#,
            )
            .unwrap();
            let env = env_for(home.path());
            let repo_root = repo.path().canonicalize().unwrap();

            let config = configure(&env, repo.path()).unwrap();

            assert_eq!(config.log_path(), repo_root.join("logs/dotr.err.log"));
            assert_eq!(config.log_level(), "info");
        }

        #[test]
        fn status_reports_stopped_after_configure_without_pid() {
            let home = tempdir().unwrap();
            let repo = tempdir().unwrap();
            fs::write(repo.path().join("dotr.toml"), "").unwrap();
            let env = env_for(home.path());

            configure(&env, repo.path()).unwrap();
            let status = status(&env).unwrap();

            assert!(status.configured);
            assert_eq!(status.state, DaemonState::Stopped);
            assert_eq!(status.pid, None);
        }

        #[test]
        fn status_reports_stale_pid() {
            let home = tempdir().unwrap();
            let repo = tempdir().unwrap();
            fs::write(repo.path().join("dotr.toml"), "").unwrap();
            let env = env_for(home.path());

            configure(&env, repo.path()).unwrap();
            let paths = daemon_paths(&env);
            fs::write(&paths.pid_file, format!("{}\n", u32::MAX)).unwrap();
            let status = status(&env).unwrap();

            assert_eq!(status.state, DaemonState::StalePid);
            assert_eq!(status.pid, Some(u32::MAX));
        }

        #[test]
        fn parse_pid_rejects_zero() {
            let err = parse_pid("0").unwrap_err();

            assert!(err.to_string().contains("invalid daemon pid"));
        }
    }
}

#[cfg(unix)]
pub use imp::*;

#[cfg(not(unix))]
mod imp {
    use std::path::{Path, PathBuf};

    use anyhow::{Result, bail};

    use crate::environment::Environment;

    pub const DAEMON_NAME: &str = "dotr-watch";

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StartReport {
        pub name: &'static str,
        pub pid: u32,
        pub already_running: bool,
        pub stdout_log: PathBuf,
        pub stderr_log: PathBuf,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StopReport {
        pub name: &'static str,
        pub pid: Option<u32>,
        pub stopped: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DaemonState {
        NotConfigured,
        Running,
        Stopped,
        StalePid,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StatusReport {
        pub name: &'static str,
        pub config: PathBuf,
        pub configured: bool,
        pub state: DaemonState,
        pub pid: Option<u32>,
        pub repo_root: Option<PathBuf>,
        pub stdout_log: PathBuf,
        pub stderr_log: PathBuf,
    }

    pub fn start(_env: &Environment, _repo_root: Option<&Path>) -> Result<StartReport> {
        bail!("dotr daemon is only supported on Unix-like systems")
    }

    pub fn stop(_env: &Environment) -> Result<StopReport> {
        bail!("dotr daemon is only supported on Unix-like systems")
    }

    pub fn status(_env: &Environment) -> Result<StatusReport> {
        bail!("dotr daemon is only supported on Unix-like systems")
    }

    pub fn is_configured(_env: &Environment) -> bool {
        false
    }
}

#[cfg(not(unix))]
pub use imp::*;
