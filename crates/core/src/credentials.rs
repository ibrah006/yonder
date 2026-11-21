//! Borrowed git credentials: a job started with `borrow_git` gets git logins from the
//! device that started it, while that device follows the job. Nothing is saved on the
//! machine.
//!
//! How it travels:
//!
//! 1. The runner points git at Yonder's helper for that job only (`GIT_CONFIG_*`
//!    environment variables), after clearing any helpers the machine has, so git neither
//!    reads nor saves the machine's own logins.
//! 2. When git needs a login it runs `yonder-agent git-credential <job folder> get`. The
//!    helper writes the request (host, protocol, path; never a password) into the job's
//!    `credentials/` folder and waits.
//! 3. The agent sees the request while the controller follows the job's log and passes
//!    it along. The controller asks its own git (`git credential fill`, so the Mac's
//!    keychain) and sends the answer back.
//! 4. The agent writes the answer next to the request; the helper reads it, deletes it,
//!    and hands it to git. If nobody answers in time, git gets nothing and fails.
//!
//! `store` and `erase` requests are ignored, so git never keeps the borrowed login.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Folder in a job's folder where requests and answers meet.
const DIR: &str = "credentials";
/// How long a job's git waits for an answer.
pub const WAIT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(200);

/// What git asks for; never contains a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCredentialRequest {
    /// Identifies the request within its job.
    pub id: String,
    pub protocol: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub username: Option<String>,
}

impl GitCredentialRequest {
    /// "https://github.com/org/repo" or as much of it as git said.
    pub fn describe(&self) -> String {
        let mut text = String::new();
        if let Some(protocol) = &self.protocol {
            text.push_str(protocol);
            text.push_str("://");
        }
        if let Some(user) = &self.username {
            text.push_str(user);
            text.push('@');
        }
        text.push_str(self.host.as_deref().unwrap_or("?"));
        if let Some(path) = &self.path {
            text.push('/');
            text.push_str(path);
        }
        text
    }

    /// Whether this device may answer it for a job allowed `hosts`: over https only, for
    /// one of those hosts, with nothing that could add lines to git's input.
    pub fn lendable(&self, hosts: &[String]) -> std::result::Result<(), String> {
        let fields = [&self.protocol, &self.host, &self.path, &self.username];
        if fields
            .iter()
            .any(|v| v.as_deref().is_some_and(|v| v.contains(['\n', '\r', '\0'])))
        {
            return Err("the request has line breaks in it".into());
        }
        if self.protocol.as_deref() != Some("https") {
            return Err("only https logins are lent".into());
        }
        let host = self
            .host
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
            return Err(format!(
                "{host} isn't one of the project's remotes ({})",
                if hosts.is_empty() {
                    "none".to_string()
                } else {
                    hosts.join(", ")
                }
            ));
        }
        Ok(())
    }

    /// The request in git's credential format, for `git credential fill`.
    pub fn to_git_input(&self) -> String {
        let mut out = String::new();
        for (key, value) in [
            ("protocol", &self.protocol),
            ("host", &self.host),
            ("path", &self.path),
            ("username", &self.username),
        ] {
            if let Some(value) = value {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
        out.push('\n');
        out
    }
}

/// A login lent to a job.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCredential {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for GitCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCredential")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl GitCredential {
    /// Reads `username=` and `password=` from `git credential fill` output.
    pub fn from_git_output(text: &str) -> Option<Self> {
        let fields = parse_fields(text);
        Some(GitCredential {
            username: fields.get("username")?.clone(),
            password: fields.get("password")?.clone(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Answer {
    credential: Option<GitCredential>,
}

/// Request ids are short lowercase hex, so they can't name another file.
pub fn valid_request_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn parse_fields(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        // Git never sends these with newlines; refuse anything that could smuggle lines.
        .filter(|(_, v)| !v.contains(['\r', '\0']))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn request_path(job_dir: &Path, id: &str) -> PathBuf {
    job_dir.join(DIR).join(format!("{id}.request"))
}

fn answer_path(job_dir: &Path, id: &str) -> PathBuf {
    job_dir.join(DIR).join(format!("{id}.answer"))
}

/// Environment for a job's git: only Yonder's helper for this job, no terminal prompts.
pub fn helper_env(agent_exe: &Path, job_dir: &Path) -> Vec<(String, String)> {
    // Git runs helpers through sh (bundled with Git for Windows); forward slashes and
    // single quotes keep paths with spaces in one piece.
    let quote = |p: &Path| {
        format!(
            "'{}'",
            p.to_string_lossy()
                .replace('\\', "/")
                .replace('\'', "'\\''")
        )
    };
    let helper = format!("!{} git-credential {}", quote(agent_exe), quote(job_dir));
    vec![
        ("GIT_CONFIG_COUNT".into(), "2".into()),
        // An empty helper clears the machine's own helpers (and their saved logins).
        ("GIT_CONFIG_KEY_0".into(), "credential.helper".into()),
        ("GIT_CONFIG_VALUE_0".into(), String::new()),
        ("GIT_CONFIG_KEY_1".into(), "credential.helper".into()),
        ("GIT_CONFIG_VALUE_1".into(), helper),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GCM_INTERACTIVE".into(), "never".into()),
    ]
}

/// `yonder-agent git-credential <job folder> <operation>`: what git runs. Reads git's
/// request from `input` and, for `get`, prints the borrowed login to `output`.
pub fn run_helper(
    job_dir: &Path,
    operation: &str,
    input: impl BufRead,
    mut output: impl Write,
    mut note: impl Write,
) -> Result<()> {
    if operation != "get" {
        // store / erase: the login was borrowed; keep nothing.
        return Ok(());
    }
    let mut text = String::new();
    for line in input.lines() {
        let line = line.map_err(|e| Error::io(job_dir, e))?;
        if line.is_empty() {
            break;
        }
        text.push_str(&line);
        text.push('\n');
    }
    let fields = parse_fields(&text);
    let id = data_encoding::HEXLOWER.encode(&rand::random::<[u8; 8]>());
    let request = GitCredentialRequest {
        id: id.clone(),
        protocol: fields.get("protocol").cloned(),
        host: fields.get("host").cloned(),
        path: fields.get("path").cloned(),
        username: fields.get("username").cloned(),
    };
    let _ = writeln!(
        note,
        "[yonder] git needs a login for {}; asking the device that started this job…",
        request.describe()
    );
    let path = request_path(job_dir, &id);
    crate::jobs::write_json(&path, &request)?;
    let answer = answer_path(job_dir, &id);
    let deadline = Instant::now() + WAIT;
    let result = loop {
        if let Ok(bytes) = std::fs::read(&answer) {
            let _ = std::fs::remove_file(&answer);
            break serde_json::from_slice::<Answer>(&bytes).ok();
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(POLL);
    };
    let _ = std::fs::remove_file(&path);
    match result.and_then(|a| a.credential) {
        Some(credential) => {
            writeln!(output, "username={}", credential.username)
                .and_then(|()| writeln!(output, "password={}", credential.password))
                .map_err(|e| Error::io(job_dir, e))?;
            let _ = writeln!(note, "[yonder] borrowed a login for {}", request.describe());
        }
        None => {
            let _ = writeln!(
                note,
                "[yonder] no login for {} (the device isn't following this job, declined, or \
                 has none saved); follow the job with `yonder attach` to lend one",
                request.describe()
            );
        }
    }
    Ok(())
}

/// Requests waiting in a job's folder.
pub fn pending(job_dir: &Path) -> Vec<GitCredentialRequest> {
    let Ok(entries) = std::fs::read_dir(job_dir.join(DIR)) else {
        return Vec::new();
    };
    let mut found: Vec<GitCredentialRequest> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "request"))
        .filter_map(|e| crate::jobs::read_json::<GitCredentialRequest>(&e.path()).ok())
        .filter(|r| valid_request_id(&r.id) && !answer_path(job_dir, &r.id).exists())
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Answers a waiting request (`None` declines it).
pub fn answer(job_dir: &Path, id: &str, credential: Option<GitCredential>) -> Result<bool> {
    if !valid_request_id(id) || !request_path(job_dir, id).is_file() {
        return Ok(false);
    }
    crate::jobs::write_json(&answer_path(job_dir, id), &Answer { credential })?;
    Ok(true)
}

/// The hosts (with any port) of the https remotes of the git repository at `dir`: what a
/// job building it may borrow logins for.
pub fn remote_hosts(dir: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "-v"])
        .output()
    else {
        return Vec::new();
    };
    let mut hosts: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(https_host)
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// "github.com" from "https://me@github.com/org/repo.git".
fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Removes requests and answers a job left (it ended while git was waiting).
pub fn clear(job_dir: &Path) {
    let _ = std::fs::remove_dir_all(job_dir.join(DIR));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_answered_through_the_job_folder() {
        let dir = tempfile::tempdir().unwrap();
        let job_dir = dir.path().to_path_buf();
        let helper = std::thread::spawn({
            let job_dir = job_dir.clone();
            move || {
                let input = "protocol=https\nhost=github.com\npath=org/repo.git\n\n";
                let (mut out, mut note) = (Vec::new(), Vec::new());
                run_helper(&job_dir, "get", input.as_bytes(), &mut out, &mut note).unwrap();
                (
                    String::from_utf8(out).unwrap(),
                    String::from_utf8(note).unwrap(),
                )
            }
        });
        let request = loop {
            if let Some(r) = pending(&job_dir).pop() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(request.host.as_deref(), Some("github.com"));
        assert_eq!(request.describe(), "https://github.com/org/repo.git");
        let credential = GitCredential {
            username: "me".into(),
            password: "pat-123".into(),
        };
        assert!(answer(&job_dir, &request.id, Some(credential)).unwrap());
        let (out, note) = helper.join().unwrap();
        assert_eq!(out, "username=me\npassword=pat-123\n");
        assert!(note.contains("borrowed"));
        // Nothing is left behind.
        assert!(pending(&job_dir).is_empty());
        let left: Vec<_> = std::fs::read_dir(job_dir.join(DIR))
            .unwrap()
            .flatten()
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn store_and_erase_keep_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for op in ["store", "erase"] {
            let input = "host=github.com\nusername=me\npassword=x\n\n";
            run_helper(dir.path(), op, input.as_bytes(), Vec::new(), Vec::new()).unwrap();
        }
        assert!(!dir.path().join(DIR).exists());
    }

    #[test]
    fn only_the_projects_https_hosts_are_lent() {
        let request = |protocol: &str, host: &str| GitCredentialRequest {
            id: "0".into(),
            protocol: Some(protocol.into()),
            host: Some(host.into()),
            path: None,
            username: None,
        };
        let hosts = vec!["github.com".to_string()];
        assert!(request("https", "github.com").lendable(&hosts).is_ok());
        assert!(request("https", "GitHub.com").lendable(&hosts).is_ok());
        assert!(request("https", "gitlab.com").lendable(&hosts).is_err());
        assert!(request("http", "github.com").lendable(&hosts).is_err());
        assert!(request("https", "github.com").lendable(&[]).is_err());
        assert!(
            request("https", "github.com\nhost=evil.com")
                .lendable(&hosts)
                .is_err()
        );
        assert_eq!(
            https_host("https://me@GitHub.com/org/repo.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            https_host("https://git.example.com:8443/r").as_deref(),
            Some("git.example.com:8443")
        );
        assert_eq!(https_host("git@github.com:org/repo.git"), None);
    }

    #[test]
    fn answers_only_go_to_real_requests() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!answer(dir.path(), "../../x", None).unwrap());
        assert!(!answer(dir.path(), "abcd", None).unwrap());
        assert!(!valid_request_id("ABCD"));
        assert!(valid_request_id("0f3a"));
    }

    #[test]
    fn git_output_and_input() {
        let c = GitCredential::from_git_output("protocol=https\nhost=x\nusername=me\npassword=p\n")
            .unwrap();
        assert_eq!((c.username.as_str(), c.password.as_str()), ("me", "p"));
        assert!(!format!("{c:?}").contains('p') || !format!("{c:?}").contains("\"p\""));
        assert!(GitCredential::from_git_output("host=x\n").is_none());
        let r = GitCredentialRequest {
            id: "a".into(),
            protocol: Some("https".into()),
            host: Some("github.com".into()),
            path: None,
            username: None,
        };
        assert_eq!(r.to_git_input(), "protocol=https\nhost=github.com\n\n");
    }

    #[test]
    fn helper_env_quotes_paths() {
        let env = helper_env(
            Path::new(r"C:\Program Files\Yonder\yonder-agent.exe"),
            Path::new(r"C:\ProgramData\Yonder\agent\jobs\0123"),
        );
        let helper = &env
            .iter()
            .find(|(k, _)| k == "GIT_CONFIG_VALUE_1")
            .unwrap()
            .1;
        assert_eq!(
            helper,
            "!'C:/Program Files/Yonder/yonder-agent.exe' git-credential 'C:/ProgramData/Yonder/agent/jobs/0123'"
        );
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "GIT_CONFIG_VALUE_0")
                .unwrap()
                .1,
            ""
        );
    }
}
