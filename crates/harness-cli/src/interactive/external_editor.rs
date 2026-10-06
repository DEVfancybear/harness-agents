//! prime-agent's external-editor handoff (`app.editor.external`, Ctrl+G): the
//! terminal goes to the `$VISUAL`/`$EDITOR` child on a temp file seeded with
//! the draft, and the saved text comes back as the draft. The terminal that
//! shows the app runs it, so an agent in the background worker edits on the
//! user's own terminal.

use std::path::PathBuf;

/// prime-agent's warning when no editor is configured.
pub const NO_EDITOR: &str =
    "\u{26a0} No editor configured. Set $VISUAL or $EDITOR environment variable.";

/// The editor command: `$VISUAL`, else `$EDITOR`; `None` when neither is set
/// to something.
#[must_use]
pub fn editor_command() -> Option<String> {
    let configured = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|command| !command.trim().is_empty())
    };
    configured("VISUAL").or_else(|| configured("EDITOR"))
}

/// Run `command` on a fresh temp file holding `text` and return what was
/// saved, one trailing newline stripped. `Ok(None)` when the editor exited
/// non-zero (the draft stays as it was). The file is created new - a path
/// already there is never followed or truncated - and removed afterwards.
///
/// # Errors
/// The draft could not be written or read back, or the editor not started.
pub fn edit(command: &str, text: &str) -> Result<Option<String>, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    let path: PathBuf = std::env::temp_dir().join(format!("ha-editor-{millis}.md"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // The handle is closed before the editor runs: on Windows a held-open
    // file cannot be rewritten by the child.
    let written = (|| -> std::io::Result<()> {
        let mut file = options.open(&path)?;
        std::io::Write::write_all(&mut file, text.as_bytes())
    })();
    if let Err(error) = written {
        if !matches!(error.kind(), std::io::ErrorKind::AlreadyExists) {
            let _ = std::fs::remove_file(&path);
        }
        return Err(format!(
            "could not write the editor draft to {}: {error}",
            path.display()
        ));
    }
    // prime-agent splits the command on spaces (program, then its arguments,
    // then the file) and shells out on Windows.
    #[cfg(not(windows))]
    let mut editor = {
        let mut tokens = command.split(' ').filter(|token| !token.is_empty());
        let mut editor = std::process::Command::new(tokens.next().unwrap_or_default());
        editor.args(tokens);
        editor.arg(&path);
        editor
    };
    // The command line goes to `cmd` as written: quoted as one argument, a
    // quoted program path (`"C:\Program Files\...\code" --wait`) would reach
    // `cmd` with its quotes escaped and fail.
    #[cfg(windows)]
    let mut editor = {
        use std::os::windows::process::CommandExt;
        let mut editor = std::process::Command::new("cmd");
        editor.raw_arg(format!("/C {command} \"{}\"", path.display()));
        editor
    };
    let outcome = match editor.status() {
        Err(error) => Err(format!(
            "could not run the external editor `{command}`: {error}"
        )),
        Ok(status) if !status.success() => Ok(None),
        Ok(_) => std::fs::read_to_string(&path)
            .map(|saved| {
                let saved = saved.replace("\r\n", "\n");
                Some(saved.strip_suffix('\n').unwrap_or(&saved).to_owned())
            })
            .map_err(|error| {
                format!(
                    "could not read the edited draft back from {}: {error}",
                    path.display()
                )
            }),
    };
    let _ = std::fs::remove_file(&path);
    outcome
}

#[cfg(test)]
mod tests {
    use super::edit;

    #[test]
    fn the_saved_file_becomes_the_draft() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let saved = dir.path().join("saved.md");
        std::fs::write(&saved, "edited draft\n").expect("saved text");
        // A stand-in editor that saves `saved.md` over the draft file.
        #[cfg(windows)]
        let command = format!("copy /Y \"{}\"", saved.display());
        #[cfg(not(windows))]
        let command = format!("cp {}", saved.display());
        assert_eq!(
            edit(&command, "first draft").expect("editor runs"),
            Some("edited draft".to_owned())
        );
    }

    #[test]
    fn a_failing_editor_keeps_the_draft() {
        #[cfg(windows)]
        let command = "exit 3 &&";
        #[cfg(not(windows))]
        let command = "false";
        assert_eq!(edit(command, "draft").expect("editor runs"), None);
    }
}
