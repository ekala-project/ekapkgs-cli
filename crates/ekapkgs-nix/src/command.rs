use std::collections::HashMap;
use std::process::{Command, ExitStatus, Output, Stdio};

use serde::de::DeserializeOwned;

/// Builder for constructing and executing nix CLI commands.
///
/// Wraps `std::process::Command` with a fluent API tailored to nix subcommands.
#[derive(Debug, Clone)]
pub struct NixCommand {
    subcommand: Vec<String>,
    args: Vec<String>,
    env: HashMap<String, String>,
}

impl NixCommand {
    /// Create a new nix command with the given subcommand.
    ///
    /// ```no_run
    /// # use ekapkgs_nix::NixCommand;
    /// let cmd = NixCommand::new(&["build"]);
    /// ```
    pub fn new(subcommand: &[&str]) -> Self {
        Self {
            subcommand: subcommand.iter().map(|s| (*s).to_string()).collect(),
            args: Vec::new(),
            env: HashMap::new(),
        }
    }

    /// Add a single argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable.
    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env.insert(key.into(), val.into());
        self
    }

    fn build_command(&self) -> Command {
        let mut cmd = Command::new("nix");
        for sub in &self.subcommand {
            cmd.arg(sub);
        }
        for arg in &self.args {
            cmd.arg(arg);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Run and capture the full output.
    pub fn output(&self) -> Result<Output, NixError> {
        tracing::debug!(cmd = %self, "running nix command");
        let output = self
            .build_command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(NixError::Spawn)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(NixError::Failed {
                status: output.status,
                stderr,
            });
        }

        Ok(output)
    }

    /// Run and parse JSON output.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, NixError> {
        let output = self.output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout).map_err(|e| NixError::Json {
            source: e,
            output: stdout.to_string(),
        })
    }

    /// Run with inherited stdout/stderr (streaming output to terminal).
    pub fn stream(&self) -> Result<ExitStatus, NixError> {
        tracing::debug!(cmd = %self, "streaming nix command");
        let status = self
            .build_command()
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(NixError::Spawn)?;

        if !status.success() {
            return Err(NixError::Failed {
                status,
                stderr: String::new(),
            });
        }

        Ok(status)
    }

    /// Replace the current process with this nix command (exec).
    pub fn exec(self) -> Result<std::convert::Infallible, NixError> {
        tracing::debug!(cmd = %self, "exec nix command");

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = self.build_command().exec();
            Err(NixError::Spawn(err))
        }

        #[cfg(not(unix))]
        {
            let status = self.stream()?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

impl std::fmt::Display for NixCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "nix")?;
        for sub in &self.subcommand {
            write!(f, " {sub}")?;
        }
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NixError {
    #[error("failed to spawn nix: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("nix exited with {status}: {stderr}")]
    Failed {
        status: ExitStatus,
        stderr: String,
    },

    #[error("failed to parse nix JSON output: {source}\noutput: {output}")]
    Json {
        source: serde_json::Error,
        output: String,
    },
}
