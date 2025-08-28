use cargo_edit::CargoResult;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(bin_name = "cargo")]
pub(crate) enum Command {
    Rm(crate::rm::RmArgs),
}

impl Command {
    pub(crate) fn exec(self) -> CargoResult<()> {
        match self {
            Self::Rm(add) => add.exec(),
        }
    }
}

#[test]
fn verify_app() {
    use clap::CommandFactory;
    Command::command().debug_assert();
}
