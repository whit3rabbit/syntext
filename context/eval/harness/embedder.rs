//! An [`Embedder`] backed by a subprocess speaking JSON lines, such as
//! `context/eval/embed_server.py`. Keeps model runtimes out of the crate:
//! the eval runner measures any model a script can load.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::Deserialize;
use syntext_context::Embedder;

#[derive(Deserialize)]
struct Hello {
    id: String,
    dims: usize,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    vectors: Vec<Vec<f32>>,
    #[serde(default)]
    error: Option<String>,
}

struct Pipe {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// One long-lived embedding process, used by every set in a run.
pub struct CommandEmbedder {
    id: String,
    dims: usize,
    pipe: Mutex<Pipe>,
}

impl CommandEmbedder {
    /// Start `command` (split on whitespace) and read its greeting.
    pub fn spawn(command: &str) -> Result<Self, String> {
        let mut parts = command.split_whitespace();
        let program = parts.next().ok_or("empty --embed command")?;
        let mut child = Command::new(program)
            .args(parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut line = String::new();
        stdout.read_line(&mut line).map_err(|e| e.to_string())?;
        let hello: Hello =
            serde_json::from_str(&line).map_err(|e| format!("embedder greeting {line:?}: {e}"))?;
        Ok(CommandEmbedder {
            id: hello.id,
            dims: hello.dims,
            pipe: Mutex::new(Pipe {
                child,
                stdin,
                stdout,
            }),
        })
    }

    fn call(&self, texts: &[&str], query: bool) -> Result<Vec<Vec<f32>>, String> {
        let mut pipe = self.pipe.lock().unwrap_or_else(|e| e.into_inner());
        let request = serde_json::json!({ "texts": texts, "query": query });
        writeln!(pipe.stdin, "{request}").map_err(|e| e.to_string())?;
        pipe.stdin.flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        pipe.stdout
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let reply: Reply = serde_json::from_str(&line).map_err(|e| format!("reply: {e}"))?;
        match reply.error {
            Some(e) => Err(e),
            None => Ok(reply.vectors),
        }
    }
}

impl Embedder for CommandEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn dims(&self) -> usize {
        self.dims
    }

    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.call(texts, false)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut v = self.call(&[text], true)?;
        v.pop().ok_or_else(|| "no vector".to_string())
    }
}

impl Drop for CommandEmbedder {
    fn drop(&mut self) {
        let pipe = self.pipe.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = pipe.child.kill();
        let _ = pipe.child.wait();
    }
}
