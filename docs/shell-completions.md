# Shell completions

Tapid can generate static completions from the same Clap command definitions used
for parsing and help. Supported shells are Bash, Elvish, Fish, PowerShell, and
Zsh. Generation is offline and does not read project configuration, manifests,
locks, credentials, or the package store. It writes only to stdout; it does not
edit shell profiles, execute generated code, or install completions.

Choose a shell explicitly and inspect or redirect the output yourself:

```sh
tapid completions bash > tapid.bash
tapid completions elvish > tapid.elv
tapid completions fish > tapid.fish
tapid completions powershell > tapid.ps1
tapid completions zsh > _tapid
```

Install the generated file using the completion mechanism for your shell. For
example, Fish can load a file named `tapid.fish` from
`~/.config/fish/completions/`; Zsh can load `_tapid` from a directory in
`$fpath`; Bash can source `tapid.bash` from its completion setup; and PowerShell
can dot-source `tapid.ps1` from the profile you control. Elvish users can source
the generated file from their own `rc.elv`. These are manual steps; Tapid does
not create directories or modify those files.

Completion scripts reflect the command definitions in the Tapid binary that
generated them and do not query registries or execute project code. `--json`
is not supported for this command and returns the normal versioned
`JSON_UNSUPPORTED_COMMAND` result. Shell names are UTF-8 command-line values;
unknown names are rejected before output is generated. A stdout write failure
(for example, a closed pipe) exits nonzero. Completions are suggestions only and
do not validate command behavior or runtime/platform support.
