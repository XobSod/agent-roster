//! Asking agents what roster can't know itself: their models, their accounts' limits.

use super::*;
use crate::usage::{Answer, Asked, Known, Via};

impl Roster {
    /// What lists each agent's models (`Adapter::models`), to run anywhere,
    /// a thread included (see `asker`).
    pub fn model_lister(&self) -> impl Fn(&str) -> Vec<agent::Model> + Send + 'static {
        let config = self.agents_config();
        let asker = self.asker();
        move |name: &str| agent::adapter(name).models(&config, &|q| asker.run(name, q, None))
    }

    /// What asks what the accounts have used of their limits, all at once, to
    /// run anywhere, a thread included (see `asker`). With a CLI Proxy
    /// enabled, it's asked for every account it holds; the agents of the
    /// services it holds none of are asked themselves (`Adapter::usage`), as
    /// they all are without one, in the order agents are offered. The proxy
    /// can't be asked: the agents are instead, unless it has answered before
    /// (`known`), when what it said stands. The env file is sourced once for
    /// all the agents, as the first asks: they're asked every few minutes,
    /// and an env file may ask a password manager.
    pub fn usage_reader(&self, known: Known) -> impl Fn() -> Asked + Send + 'static {
        let config = self.agents_config();
        let agents = agent::available(&self.config);
        let asker = self.asker();
        // Nothing is asked with account_limits off, whoever calls this.
        let allowed = self.config.account_limits();
        let proxy = self.config.cli_proxy().map(String::from);
        let secrets = self.paths.secrets_file();
        move || {
            if !allowed {
                return Asked {
                    via: Via::Agents,
                    answers: Vec::new(),
                };
            }
            let (via, mut answers, held) = match &proxy {
                None => (Via::Agents, Vec::new(), Vec::new()),
                Some(url) => match ask_proxy(url, &secrets, &known.plans) {
                    Ok(answers) => {
                        let mut held: Vec<String> =
                            answers.iter().map(|a| a.agent.clone()).collect();
                        held.sort();
                        held.dedup();
                        (Via::Proxy(url.clone()), answers, held)
                    }
                    Err(e) => {
                        let (url, why) = (url.clone(), format!("{e:#}"));
                        if known.answered {
                            (Via::Failed { url, why }, Vec::new(), known.held.clone())
                        } else {
                            (Via::FellBack { url, why }, Vec::new(), Vec::new())
                        }
                    }
                },
            };
            let agents: Vec<&String> = agents.iter().filter(|a| !held.contains(a)).collect();
            answers.extend(ask_agents(&agents, &config, &asker));
            Asked { via, answers }
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

/// Every account the CLI Proxy at `url` holds, and what each has used: its
/// key read from `secrets` now, so one set since counts. Each account is
/// asked at once, Claude's for its plan too unless `plans` has it; one that
/// fails doesn't fail the others.
fn ask_proxy(
    url: &str,
    secrets: &Path,
    plans: &std::collections::HashMap<String, String>,
) -> Result<Vec<Answer>> {
    let key = crate::secrets::proxy_key(secrets)?
        .context("its key isn't set (roster config set cli_proxy.key)")?;
    let proxy = crate::proxy::Proxy::new(url, key);
    let accounts = proxy.accounts()?;
    let answers = std::thread::scope(|scope| {
        let asking: Vec<_> = accounts
            .into_iter()
            .map(|account| {
                let proxy = &proxy;
                let asked = account.clone();
                let said = scope.spawn(move || -> Result<Option<crate::usage::Usage>> {
                    let account = &asked;
                    if account.disabled || !account.asked() {
                        return Ok(None);
                    }
                    if account.sign_in {
                        bail!("the CLI Proxy says it needs signing in again");
                    }
                    // Its plan once its usage has answered: not asked of
                    // one that can't say (a round later, again).
                    let usage = proxy.usage(account)?;
                    Ok(usage.map(|mut u| {
                        if u.plan.is_none() && account.provider == "claude" {
                            u.plan = (plans.get(&account.id).cloned())
                                .or_else(|| proxy.claude_plan(account).ok().flatten());
                        }
                        u
                    }))
                });
                (account, said)
            })
            .collect();
        asking
            .into_iter()
            .map(|(account, said)| Answer {
                agent: account.provider.clone(),
                said: said.join().unwrap_or_else(|panic| {
                    Err(anyhow::anyhow!(
                        "roster failed asking it: {}",
                        util::panic_message(&*panic)
                    ))
                }),
                account: Some(account),
            })
            .collect::<Vec<_>>()
    });
    Ok(answers)
}

/// What `agents` say their own accounts have used, each asked at once, in
/// their order: those that have such an account.
fn ask_agents(agents: &[&String], config: &Config, asker: &Asker) -> Vec<Answer> {
    if agents.is_empty() {
        return Vec::new();
    }
    // Sourced as the first agent asks; what it started ends with the round,
    // as this goes.
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
                let ask = &ask;
                let said =
                    scope.spawn(move || agent::adapter(name).usage(config, &|q| ask(name, q)));
                (name, said)
            })
            .collect();
        let mut answers: Vec<_> = asking
            .into_iter()
            .filter_map(|(name, said)| asked(name, said.join()))
            .collect();
        // The env file failing is said as that, not as what each agent
        // couldn't do for it.
        if let Some(Err(e)) = env.get() {
            for (_, said) in &mut answers {
                if said.is_err() {
                    *said = Err(anyhow::anyhow!("{e}"));
                }
            }
        }
        answers
            .into_iter()
            .map(|(agent, said)| Answer {
                agent,
                account: None,
                said: said.map(Some),
            })
            .collect()
    })
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
