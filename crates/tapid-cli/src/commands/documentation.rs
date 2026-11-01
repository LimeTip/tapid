use clap::{Command, CommandFactory};

#[test]
fn project_exec_preserves_command_and_opaque_arguments_after_separator() {
    use clap::Parser;
    let cli = super::Cli::try_parse_from([
        "tapid",
        "exec",
        "--package",
        "typescript",
        "--",
        "tsc",
        "",
        "a b",
        "$(touch marker)",
        "--noEmit",
    ])
    .unwrap();
    let Some(super::Command::Exec(args)) = cli.command else {
        panic!("expected exec")
    };
    assert_eq!(args.command.as_deref(), Some("tsc"));
    assert_eq!(args.arguments, ["", "a b", "$(touch marker)", "--noEmit"]);
}

fn undocumented_commands(command: &Command, path: &str, missing: &mut Vec<String>) {
    if command.is_hide_set() {
        return;
    }

    if command
        .get_about()
        .is_none_or(|about| about.to_string().trim().is_empty())
    {
        missing.push(path.to_owned());
    }

    for subcommand in command.get_subcommands() {
        undocumented_commands(
            subcommand,
            &format!("{path} {}", subcommand.get_name()),
            missing,
        );
    }
}

#[test]
fn all_cli_commands_have_help_descriptions() {
    // Inspect declared commands before Clap adds its generated help subcommand.
    let command = super::Cli::command();
    let mut missing = Vec::new();
    undocumented_commands(&command, command.get_name(), &mut missing);
    assert!(
        missing.is_empty(),
        "CLI commands missing help descriptions:\n{}\nAdd a doc comment or #[command(about = \"...\")] to each command.",
        missing.join("\n")
    );
}

#[test]
fn documentation_check_reports_missing_and_blank_nested_descriptions() {
    let command = Command::new("tapid").about("Package manager").subcommand(
        Command::new("manifest")
            .subcommand(Command::new("validate").about(" \n "))
            .subcommand(Command::new("show").about("Show the manifest")),
    );
    let mut missing = Vec::new();
    undocumented_commands(&command, command.get_name(), &mut missing);
    assert_eq!(missing, ["tapid manifest", "tapid manifest validate"]);
}

#[test]
fn documentation_check_skips_hidden_commands_and_their_children() {
    let command = Command::new("tapid").about("Package manager").subcommand(
        Command::new("private")
            .hide(true)
            .subcommand(Command::new("child")),
    );
    let mut missing = Vec::new();
    undocumented_commands(&command, command.get_name(), &mut missing);
    assert!(missing.is_empty());
}
