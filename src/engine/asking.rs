//! Asking agents what roster can't know itself: their models, their accounts' limits.

use super::*;

impl Roster {
    /// What lists each agent's models (`Adapter::models`), to run anywhere,
    /// a thread included (see `asker`).
    pub fn model_lister(&self) -> impl Fn(&str) -> Vec<agent::Model> + Send + 'static {
        let config = self.agents_config();
        let asker = self.asker();
        move |name: &str| agent::adapter(name).models(&config, &|q| asker.run(name, q, None))
    }

    /// What asks every agent that can be started what its account has used
    /// of its limits (`Adapter::usage`), all at once, to run anywhere, a
    /// thread included (see `asker`): those that say, in the order agents are
    /// offered (the default first). The env file is sourced once for all of
    /// them, as the first asks: they're asked every few minutes, and an env
    /// file may ask a password manager.
    pub fn usage_reader(
        &self,
    ) -> impl Fn() -> Vec<(String, Result<crate::usage::Usage>)> + Send + 'static {
        let config = self.agents_config();
        let agents = agent::available(&self.config);
        let asker = self.asker();
        // Nothing is asked with account_limits off, whoever calls this.
        let allowed = self.config.account_limits();
        move || {
            if !allowed {
                return Vec::new();
            }
            // Sourced as the first agent asks; what it started ends with
            // the round, as this goes.
            let env: std::sync::OnceLock<std::result::Result<launch::Environment, String>> =
                std::sync::OnceLock::new();
            let ask = |name: &str, question: agent::Question<'_>| match env
                .get_or_init(|| asker.environment().map_err(|e| format!("{e:#}")))
            {
                Ok(env) => asker.run(name, question, Some(env)),
                Err(e) => Err(anyhow::anyhow!("{e}")),
            };
            std::thread::scope(|scope| {
                let asking: Vec<_> = agents
                    .iter()
                    .map(|name| {
                        let (config, ask) = (&config, &ask);
                        let said = scope
                            .spawn(move || agent::adapter(name).usage(config, &|q| ask(name, q)));
                        (name, said)
                    })
                    .collect();
                let mut answers: Vec<_> = asking
                    .into_iter()
                    .filter_map(|(name, said)| asked(name, said.join()))
                    .collect();
                // The env file failing is said as that, not as what each
                // agent couldn't do for it.
                if let Some(Err(e)) = env.get() {
                    for (_, said) in &mut answers {
                        if said.is_err() {
                            *said = Err(anyhow::anyhow!("{e}"));
                        }
                    }
                }
                answers
            })
        }
    }

    /// The config's agents alone, for a thread.
    pub(super) fn agents_config(&self) -> Config {
        Config {
            agents: self.config.agents.clone(),
            ..Config::default()
        }
    }

    /// What runs agents' commands as their sessions would (`Asker`).
    pub(super) fn asker(&self) -> Asker {
        Asker {
            shell: launch::shell(&self.config, std::env::var("SHELL").ok().as_deref()),
            global_env: self.paths.global_env(),
            dir: self.paths.state_dir.clone(),
        }
    }
}

/// What runs an agent's commands (`agent::Ask`) as its sessions would, to
/// run anywhere, a thread included: the global env file sourced (in roster's
/// state directory, away from any project's settings), each for at most 10
/// seconds.
pub(super) struct Asker {
    shell: String,
    global_env: PathBuf,
    dir: PathBuf,
}

impl Asker {
    /// Runs `question` for agent `name` and returns what it printed: in
    /// `env`, else with the env file sourced for it alone.
    fn run(
        &self,
        name: &str,
        question: agent::Question<'_>,
        env: Option<&launch::Environment>,
    ) -> Result<String> {
        let printed = self.printed(name, question, env)?;
        Ok(String::from_utf8_lossy(&printed).into_owned())
    }

    fn printed(
        &self,
        name: &str,
        question: agent::Question<'_>,
        env: Option<&launch::Environment>,
    ) -> Result<Vec<u8>> {
        let mut cmd = match env {
            Some(env) => {
                let mut cmd = std::process::Command::new(&question.argv[0]);
                cmd.args(&question.argv[1..])
                    .env_clear()
                    .envs(env.vars.iter().map(|(k, v)| (k, v)));
                cmd
            }
            None => {
                let argv = launch::wrap(
                    &self.shell,
                    &self.global_env,
                    Path::new(""),
                    None,
                    &question.argv,
                );
                let mut cmd = std::process::Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                for var in crate::tmux::LEAKY_ENV {
                    cmd.env_remove(var);
                }
                cmd
            }
        };
        cmd.current_dir(&self.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = launch::converse(cmd, name, Duration::from_secs(10), question.input)?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            match said.lines().rfind(|l| !l.trim().is_empty()) {
                Some(line) => bail!("{}", line.trim()),
                None => bail!("{} exited with {}", question.argv[0], output.status),
            }
        }
        Ok(output.stdout)
    }

    /// The variables the env file makes, for several commands to run in, so
    /// it's sourced once for all of them (`launch::environment`).
    fn environment(&self) -> Result<launch::Environment> {
        launch::environment(
            &self.shell,
            &self.global_env,
            &self.dir,
            Duration::from_secs(10),
        )
    }
}

/// What asking agent `name` what its account has used came to: its answer;
/// none without an account; or, where asking it failed on a mistake of
/// roster's (its thread panicked), that, rather than seeming to have none.
pub(super) fn asked(
    name: &str,
    joined: std::thread::Result<Option<Result<crate::usage::Usage>>>,
) -> Option<(String, Result<crate::usage::Usage>)> {
    let said = match joined {
        Ok(said) => said?,
        Err(panic) => Err(anyhow::anyhow!(
            "roster failed asking it: {}",
            util::panic_message(&*panic)
        )),
    };
    Some((name.to_string(), said))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_that_fails_on_a_mistake_of_rosters_says_so() {
        let panicked = std::thread::spawn(|| -> Option<Result<crate::usage::Usage>> {
            panic!("attempt to multiply with overflow")
        })
        .join();
        let (agent, said) = asked("claude", panicked).unwrap();
        assert_eq!(agent, "claude");
        assert_eq!(
            said.unwrap_err().to_string(),
            "roster failed asking it: attempt to multiply with overflow"
        );
        // No account: no answer.
        assert!(asked("pi", Ok(None)).is_none());
    }
}
