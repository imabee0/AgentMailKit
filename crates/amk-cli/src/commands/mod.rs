//! The three `amk` subcommands this dispatch implements. `import` deliberately has no module
//! here — see the dispatch contract's "`amk import` — does not exist yet" section; the correct
//! behaviour for that subcommand today is `crate::args`'s ordinary "unknown command" error, not a
//! stub that pretends to do something.

pub mod doctor;
pub mod init;
pub mod migrate;
