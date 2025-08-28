use cargo_edit::CargoResult;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(bin_name = "cargo")]
pub(crate) enum Command {
    Add(crate::add::AddArgs),
}

impl Command {
    pub(crate) fn exec(self) -> CargoResult<()> {
        match self {
            Self::Add(add) => add.exec(),
        }
    }
}

#[test]
fn verify_app() {
    use clap::CommandFactory;
    Command::command().debug_assert();
}
