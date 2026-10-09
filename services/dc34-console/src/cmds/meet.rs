// meet.rs - forwards `meet ...` lines to the vault app, which owns the meeting-link list.
//
// The protocol and all of the logic are in `apps-baosec/dc34-vault/src/meetcmd.rs`. Only this thin
// forwarder lives here because the console is part of the size-constrained RRAM image, while the
// vault is loaded from swap. The vault prints the `MEET ...` replies itself.

use crate::{CommonEnv, ShellCmdApi};

pub struct Meet {
    vault_conn: Option<xous::CID>,
}

impl Meet {
    pub fn new() -> Self { Meet { vault_conn: None } }
}

impl<'a> ShellCmdApi<'a> for Meet {
    cmd_api!(meet);

    fn process(&mut self, args: String, env: &mut CommonEnv) -> Result<Option<String>, xous::Error> {
        if self.vault_conn.is_none() {
            self.vault_conn = env.xns.request_connection_blocking(dc34_api::SERVER_NAME_VAULT2).ok();
        }
        match self.vault_conn {
            Some(conn) if dc34_api::forward_meet_cmd(conn, &args).is_ok() => Ok(None),
            _ => Ok(Some(String::from("MEET ERR vault not reachable"))),
        }
    }
}
