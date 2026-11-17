//! 一般 Shell 分頁：只區分執行中與已結束。

use crate::{AgentSettings, base_command};
use agt_core::{
    AgentAdapter, CommandSpec, LaunchCtx, OscEvent, ScreenView, StatusSourceKind, platform,
};
use agt_proto::AgentStatus;
use std::time::Duration;

pub struct ShellAdapter {
    settings: AgentSettings,
}

impl ShellAdapter {
    pub fn new(settings: AgentSettings) -> Self {
        ShellAdapter { settings }
    }
}

impl AgentAdapter for ShellAdapter {
    fn id(&self) -> &'static str {
        "shell"
    }

    fn launch(&self, ctx: &LaunchCtx) -> CommandSpec {
        let program = if self.settings.command.is_empty() {
            platform::default_shell()
        } else {
            self.settings.command.clone()
        };
        let mut spec = base_command(&program, &self.settings, ctx);
        if spec.args.is_empty() && (program == "pwsh" || program == "powershell") {
            spec.args = vec!["-NoLogo".into()];
        }
        spec
    }

    fn install_hooks(&self, _: &LaunchCtx) -> agt_core::Result<()> {
        Ok(())
    }

    fn on_osc(&mut self, _: &OscEvent) -> Option<AgentStatus> {
        None
    }

    fn on_screen(&mut self, _: &ScreenView) -> Option<AgentStatus> {
        None
    }

    fn on_idle(&mut self, _: Duration) -> Option<AgentStatus> {
        None
    }

    fn status_source(&self) -> StatusSourceKind {
        StatusSourceKind::None
    }
}
