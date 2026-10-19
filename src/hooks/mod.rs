pub mod bash;
pub mod zsh;

use crate::cache::shell_escape;
use std::path::Path;

/// Shell lines (bash and zsh) that move the shim folder to the front of PATH, dropping any copy further back: a
/// nested shell inherits it, and its startup files may put another `claude` ahead of it before the hook runs.
const SHIM_ON_PATH: &str = r#"
# A route picks the Claude Code folder when claude starts: aitch's claude shim comes first on PATH
unalias claude 2>/dev/null || true
_aitch_d='@DIR@'
_aitch_p=":${PATH:-}:"
while [ "${_aitch_p#*:"$_aitch_d":}" != "$_aitch_p" ]; do
  _aitch_p="${_aitch_p%%:"$_aitch_d":*}:${_aitch_p#*:"$_aitch_d":}"
done
if [ "$_aitch_p" = : ]; then
  PATH="$_aitch_d"
else
  _aitch_p="${_aitch_p#:}"
  PATH="$_aitch_d:${_aitch_p%:}"
fi
unset _aitch_d _aitch_p
"#;

/// Put the folder holding aitch's `claude` shim first on PATH. A PATH entry, unlike a shell function named `claude`,
/// survives terminals that install their own `claude` function (cmux does), whose wrapper then finds it.
pub fn claude_shim(dir: Option<&Path>) -> String {
    match dir {
        Some(dir) => SHIM_ON_PATH.replace("@DIR@", &shell_escape(&dir.to_string_lossy())),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn shim_folder_goes_first_once_and_other_entries_stay() {
        let d = "/h/it's a [dir]";
        let cases = [
            ("/u:/v", "/h/it's a [dir]:/u:/v"),
            (
                "/u:/h/it's a [dir]:/v:/h/it's a [dir]",
                "/h/it's a [dir]:/u:/v",
            ),
            ("/h/it's a [dir]", "/h/it's a [dir]"),
            (
                "/u:/h/it's a [dir]/x",
                "/h/it's a [dir]:/u:/h/it's a [dir]/x",
            ),
            // An empty entry is the current folder and must stay.
            ("/u::/v", "/h/it's a [dir]:/u::/v"),
            (":/h/it's a [dir]", "/h/it's a [dir]:"),
            ("/h/it's a [dir]:", "/h/it's a [dir]:"),
            ("", "/h/it's a [dir]:"),
        ];
        let snippet = claude_shim(Some(Path::new(d)));
        for shell in ["/bin/bash", "/bin/zsh"] {
            for (path, want) in cases {
                let out = Command::new(shell)
                    .arg("-c")
                    .arg(format!("{snippet}\nprintf %s \"$PATH\""))
                    .env("PATH", path)
                    .output()
                    .unwrap();
                assert_eq!(
                    String::from_utf8_lossy(&out.stdout),
                    want,
                    "{shell} PATH={path:?}"
                );
            }
        }
    }
}
