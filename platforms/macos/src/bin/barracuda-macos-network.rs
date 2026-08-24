//! Privileged macOS UTUN/NAT launcher for an unprivileged Barracuda process.

#[cfg(target_os = "macos")]
mod macos {
    use std::error::Error;
    use std::io::{self, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, ExitCode, Stdio};

    use barracuda_platform_macos::NETWORK_FD_ENV;

    const PF_ANCHOR: &str = "com.apple/barracuda";

    pub fn main() -> Result<ExitCode, Box<dyn Error>> {
        let mut arguments = std::env::args_os().skip(1);
        let program = arguments
            .next()
            .ok_or("usage: sudo barracuda-macos-network <barracuda-program> [arguments...]")?;
        let user_id = sudo_identity("SUDO_UID")?;
        let group_id = sudo_identity("SUDO_GID")?;

        let mut configuration = tun::Configuration::default();
        configuration
            .address((10, 42, 0, 1))
            .destination((10, 42, 0, 2))
            .netmask((255, 255, 255, 252))
            .mtu(tun::DEFAULT_MTU)
            .layer(tun::Layer::L3)
            .up();
        configuration.platform_config(|platform| {
            platform.packet_information(true).enable_routing(true);
        });
        let interface = tun::create(&configuration)?;
        let descriptor = interface.as_raw_fd();

        let outbound = default_interface()?;
        let forwarding = sysctl_value("net.inet.ip.forwarding")?;
        set_sysctl("net.inet.ip.forwarding", "1")?;
        let mut cleanup = NetworkCleanup {
            forwarding,
            pf_token: None,
        };
        cleanup.pf_token = Some(install_nat(&outbound)?);

        let status = Command::new(program)
            .args(arguments)
            .env(NETWORK_FD_ENV, descriptor.to_string())
            .uid(user_id)
            .gid(group_id)
            .status()?;

        drop(cleanup);
        drop(interface);
        Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }

    fn sudo_identity(name: &str) -> Result<u32, Box<dyn Error>> {
        let value = std::env::var(name)
            .map_err(|_error| format!("{name} is missing; run this launcher through sudo"))?;
        value
            .parse::<u32>()
            .map_err(|error| format!("invalid {name}: {error}").into())
    }

    fn default_interface() -> Result<String, Box<dyn Error>> {
        let output = checked(Command::new("/sbin/route").args(["-n", "get", "default"]))?;
        let text = String::from_utf8(output.stdout)?;
        text.lines()
            .find_map(|line| line.trim().strip_prefix("interface:").map(str::trim))
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "macOS default route did not report an outbound interface".into())
    }

    fn sysctl_value(name: &str) -> Result<String, Box<dyn Error>> {
        let output = checked(Command::new("/usr/sbin/sysctl").args(["-n", name]))?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn set_sysctl(name: &str, value: &str) -> Result<(), Box<dyn Error>> {
        checked(Command::new("/usr/sbin/sysctl").args(["-w", &format!("{name}={value}")]))?;
        Ok(())
    }

    fn install_nat(outbound: &str) -> Result<String, Box<dyn Error>> {
        let rule = format!("nat on {outbound} from 10.42.0.0/30 to any -> ({outbound})\n");
        let mut child = Command::new("/sbin/pfctl")
            .args(["-a", PF_ANCHOR, "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        child
            .stdin
            .as_mut()
            .ok_or("pfctl stdin was not available")?
            .write_all(rule.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("pfctl rejected Barracuda NAT rule with {status}").into());
        }
        let enable = checked(Command::new("/sbin/pfctl").arg("-E"))?;
        let output = format!(
            "{}\n{}",
            String::from_utf8_lossy(&enable.stdout),
            String::from_utf8_lossy(&enable.stderr)
        );
        parse_pf_token(&output)
            .map(str::to_owned)
            .ok_or_else(|| "pfctl enabled PF without returning a reference token".into())
    }

    fn parse_pf_token(output: &str) -> Option<&str> {
        output.lines().find_map(|line| {
            let (label, value) = line.trim().split_once(':')?;
            let value = value.trim();
            (label.trim() == "Token" && !value.is_empty()).then_some(value)
        })
    }

    fn checked(command: &mut Command) -> Result<std::process::Output, io::Error> {
        let output = command.output()?;
        if output.status.success() {
            return Ok(output);
        }
        Err(io::Error::other(format!(
            "command {:?} failed with {}: {}",
            command,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    struct NetworkCleanup {
        forwarding: String,
        pf_token: Option<String>,
    }

    impl Drop for NetworkCleanup {
        fn drop(&mut self) {
            let _result = Command::new("/sbin/pfctl")
                .args(["-a", PF_ANCHOR, "-F", "all"])
                .status();
            if let Some(token) = self.pf_token.as_deref() {
                let _result = Command::new("/sbin/pfctl").args(["-X", token]).status();
            }
            let _result = Command::new("/usr/sbin/sysctl")
                .args(["-w", &format!("net.inet.ip.forwarding={}", self.forwarding)])
                .status();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::parse_pf_token;

        #[test]
        fn parses_pf_reference_token() {
            assert_eq!(
                parse_pf_token("pf enabled\nToken : 123456\n"),
                Some("123456")
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    macos::main()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("barracuda-macos-network is only available on macOS");
    std::process::ExitCode::FAILURE
}
