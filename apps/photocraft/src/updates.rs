//! Native Komaphone update policy. A separate reviewed release channel has not been configured.
//! Never offer upstream PhotoCraft releases as updates for this independent application.

use photocraft_engine::updates_cmds::CheckFn;

pub fn service() -> Option<CheckFn> {
    None
}
