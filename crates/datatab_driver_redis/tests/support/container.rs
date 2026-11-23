use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;

pub(crate) const IMAGE: &str = "redis:7-alpine";

pub(crate) fn command(command: &mut Command) -> String {
    let output = command.output().expect("start fixture command");
    assert!(
        output.status.success(),
        "fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(crate) struct TestContainer {
    pub(crate) name: String,
}

impl TestContainer {
    pub(crate) fn start(name: &str, options: &[String], arguments: &[String]) -> Self {
        // Acquire ownership only after create succeeds; a name collision must
        // never cause Drop to remove somebody else's container.
        command(
            Command::new("docker")
                .args([
                    "create",
                    "--pull=never",
                    "--name",
                    name,
                    "--memory=256m",
                    "--tmpfs",
                    "/data",
                    "--tmpfs",
                    "/run/redistls",
                ])
                .args(options)
                .arg(IMAGE)
                .args(arguments),
        );
        let container = Self {
            name: name.to_string(),
        };
        container.record("created");
        command(Command::new("docker").args(["start", name]));
        container
    }

    fn record(&self, action: &str) {
        if let Some(scratch) = std::env::var_os("DATATAB_TEST_SCRATCH") {
            let path = std::path::PathBuf::from(scratch).join("container-events.log");
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("open container event log");
            writeln!(file, "{action}: {} ({IMAGE})", self.name).expect("write container event");
        }
    }
}

impl Drop for TestContainer {
    fn drop(&mut self) {
        let output = Command::new("docker")
            .args(["rm", "-f", "-v", &self.name])
            .output()
            .expect("remove test container");
        if output.status.success() {
            self.record("removed");
        } else {
            eprintln!("failed to remove test container {}", self.name);
            assert!(std::thread::panicking(), "test container cleanup failed");
        }
    }
}
