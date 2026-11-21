//! The text output of `ds doctor`.

use std::fmt::Write;

use super::style::{Style, Tone};
use crate::checks::finding::{Finding, Level};
use crate::discovery::ManagedPort;
use crate::kept_worktrees::KeptWorktree;
use crate::report::{
    DoctorReport, InfoPort, InfoReport, ListReport, RegisteredInfo, UnregisteredInfo,
};
use crate::shell_quote::shell_quote;

const LEVEL_WORD_WIDTH: usize = 7;
const NO_VALUE: &str = "-";
const FORCED_MARK: &str = "(forced in devslot.local.toml)";
const SERVICES_NOT_CHECKED: &str = "run ds info to check";
const KEPT_WORKTREES_HEADING: &str = "Worktrees Claude Code asked to remove that are still there:";
const NO_INSTANCE_LINE: &str = "No instance is registered on this machine.\n";

/// Renders the report as the text `ds doctor` prints.
pub fn render_text(report: &DoctorReport, style: Style) -> String {
    let mut output = String::new();
    let compose_file = report.compose_file.as_deref().unwrap_or("none");
    let _ = writeln!(
        output,
        "Checkout:      {}",
        escape_control(&report.checkout)
    );
    let _ = writeln!(output, "Compose file:  {}", escape_control(compose_file));
    output.push_str("\nPorts\n");
    output.push_str(&render_ports(&report.ports));
    output.push_str("\nProblems\n");
    output.push_str(&render_findings(&report.findings, style));
    let _ = write!(
        output,
        "\n{}, {}\n",
        count_phrase(report.summary.errors, "error"),
        count_phrase(report.summary.warnings, "warning")
    );
    output
}

fn count_phrase(count: usize, noun: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {noun}{plural}")
}

fn port_row(port: &ManagedPort) -> [String; 4] {
    let default = port
        .default
        .map_or_else(|| NO_VALUE.to_owned(), |value| value.to_string());
    let target = match (&port.service, port.target) {
        (Some(service), Some(target)) => format!("{service}:{target}"),
        (Some(service), None) => service.clone(),
        (None, _) => NO_VALUE.to_owned(),
    };
    [
        escape_control(&port.variable),
        escape_control(&port.file),
        default,
        escape_control(&target),
    ]
}

fn render_ports(ports: &[ManagedPort]) -> String {
    if ports.is_empty() {
        return "  none found\n".to_owned();
    }
    let header = ["VARIABLE", "SOURCE", "DEFAULT", "SERVICE"].map(str::to_owned);
    render_table(header, ports.iter().map(port_row).collect())
}

fn render_table<const COLUMNS: usize>(
    header: [String; COLUMNS],
    body: Vec<[String; COLUMNS]>,
) -> String {
    let rows: Vec<[String; COLUMNS]> = std::iter::once(header).chain(body).collect();
    let widths: [usize; COLUMNS] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    });
    let mut output = String::new();
    for row in &rows {
        let cells: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        let _ = writeln!(output, "  {}", cells.join("  ").trim_end());
    }
    output
}

/// The findings as indented lines, one each; "none" when there is no finding.
pub fn render_findings(findings: &[Finding], style: Style) -> String {
    if findings.is_empty() {
        return "  none\n".to_owned();
    }
    let mut output = String::new();
    for finding in findings {
        let _ = writeln!(output, "  {}", render_finding(finding, style));
    }
    output
}

/// The text with every control character (ESC, CR, BEL, C1) replaced by its
/// visible escape, so a repository cannot move the cursor, hide a line or
/// write to the clipboard through a report.
fn escape_control(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                character.escape_debug().to_string()
            } else {
                character.to_string()
            }
        })
        .collect()
}

fn render_finding(finding: &Finding, style: Style) -> String {
    let word = format!("{:<LEVEL_WORD_WIDTH$}", finding.level.word());
    let tone = match finding.level {
        Level::Error => Tone::Bad,
        Level::Warning => Tone::Warning,
        Level::Info => Tone::Muted,
    };
    let location = match (&finding.file, &finding.service) {
        (Some(file), Some(service)) => format!("{file} ({service}): "),
        (Some(file), None) => format!("{file}: "),
        (None, _) => String::new(),
    };
    format!(
        "{} {}{}",
        style.paint(tone, &word),
        escape_control(&location),
        escape_control(&finding.message)
    )
}

/// Renders the answer of `ds ls` as text: one row per instance, the ports of
/// each, then the checkouts `ds rm` opted out.
pub fn render_list_text(report: &ListReport, opted_out: &[String]) -> String {
    if report.instances.is_empty() && opted_out.is_empty() {
        return NO_INSTANCE_LINE.to_owned();
    }
    let mut output = String::new();
    if report.instances.is_empty() {
        output.push_str(NO_INSTANCE_LINE);
    } else {
        output.push_str(&render_instance_table(report));
    }
    for instance in &report.instances {
        let _ = write!(
            output,
            "\nPorts of {}\n{}",
            instance.name,
            render_info_ports(&instance.ports)
        );
    }
    if !opted_out.is_empty() {
        output.push_str("\nOpted out\n");
        for path in opted_out {
            let _ = writeln!(output, "  {path}");
        }
    }
    output
}

/// Renders the worktrees Claude Code asked to remove that are still there:
/// nothing when there are none.
pub fn render_kept_worktrees(kept: &[KeptWorktree]) -> String {
    if kept.is_empty() {
        return String::new();
    }
    let mut output = format!("\n{KEPT_WORKTREES_HEADING}\n");
    for worktree in kept {
        let path = worktree.path.display().to_string();
        let _ = writeln!(
            output,
            "  {path}: {} ({}); remove with ds worktree rm {}",
            worktree.reason,
            worktree.date,
            shell_quote(&path)
        );
    }
    output
}

fn render_instance_table(report: &ListReport) -> String {
    let header = ["NAME", "SLOT", "SERVICES", "PATH"].map(str::to_owned);
    let rows = report
        .instances
        .iter()
        .map(|instance| {
            let services = match instance.services_running {
                Some(true) => "running",
                Some(false) => "not running",
                None => "unknown",
            };
            [
                instance.name.clone(),
                instance.slot.to_string(),
                services.to_owned(),
                instance.path.clone(),
            ]
        })
        .collect();
    render_table(header, rows)
}

/// Renders the answer of `ds info` as text.
pub fn render_info_text(report: &InfoReport) -> String {
    match report {
        InfoReport::Registered(info) => render_registered_info(info),
        InfoReport::Unregistered(info) => render_unregistered_info(info),
    }
}

fn render_unregistered_info(info: &UnregisteredInfo) -> String {
    if info.opted_out {
        return format!(
            "Path:          {}\nThis checkout is opted out by ds rm; ds up or ds run registers it again.\n",
            info.path
        );
    }
    format!(
        "Path:          {}\nThis checkout is not registered. Run ds up to register it.\n",
        info.path
    )
}

fn render_registered_info(info: &RegisteredInfo) -> String {
    render_registered_info_with(info, info.services.word())
}

/// Renders a registered instance like `ds info` does, but without the state
/// of the services, which needs a Docker call: it says how to check instead.
pub fn render_registered_info_unchecked(info: &RegisteredInfo) -> String {
    render_registered_info_with(info, SERVICES_NOT_CHECKED)
}

fn render_registered_info_with(info: &RegisteredInfo, services: &str) -> String {
    let compose = match (&info.compose_file, &info.compose_file_origin) {
        (Some(file), Some(origin)) => format!("{file} ({origin})"),
        (Some(file), None) => file.clone(),
        (None, _) => "none".to_owned(),
    };
    let mut output = String::new();
    let _ = writeln!(output, "Name:          {}", info.name);
    let _ = writeln!(output, "Path:          {}", info.path);
    let _ = writeln!(
        output,
        "Slot:          {} ({}-{})",
        info.slot, info.range[0], info.range[1]
    );
    let _ = writeln!(output, "Host:          {}", info.host);
    if let Some(app_url) = &info.app_url {
        let _ = writeln!(output, "App URL:       {app_url}");
    }
    let _ = writeln!(output, "Compose file:  {compose}");
    let _ = writeln!(output, "Services:      {services}");
    output.push_str("\nPorts\n");
    output.push_str(&render_info_ports(&info.ports));
    output
}

fn port_text(port: &InfoPort) -> String {
    if port.forced {
        return format!("{} {FORCED_MARK}", port.port);
    }
    port.port.to_string()
}

fn render_info_ports(ports: &[InfoPort]) -> String {
    if ports.is_empty() {
        return "  none\n".to_owned();
    }
    let header = ["VARIABLE", "PORT", "KIND", "SERVICE", "URL"].map(str::to_owned);
    let rows = ports
        .iter()
        .map(|port| {
            let target = match (&port.service, port.target) {
                (Some(service), Some(target)) => format!("{service}:{target}"),
                (Some(service), None) => service.clone(),
                (None, _) => NO_VALUE.to_owned(),
            };
            [
                port.variable.clone(),
                port_text(port),
                port.kind.word().to_owned(),
                target,
                port.url.clone().unwrap_or_else(|| NO_VALUE.to_owned()),
            ]
        })
        .collect();
    render_table(header, rows)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::checks::finding::{Check, Summary};
    use crate::discovery::{PortKind, PortSource};
    use crate::services_state::ServicesState;

    fn plain() -> Style {
        Style { color: false }
    }

    fn report(ports: Vec<ManagedPort>, findings: Vec<Finding>) -> DoctorReport {
        DoctorReport {
            version: 1,
            checkout: "/work/app".to_owned(),
            compose_file: Some("dev.compose.yml".to_owned()),
            summary: Summary::of(&findings),
            ports,
            findings,
        }
    }

    #[test]
    fn should_render_no_control_character_when_a_finding_holds_escape_and_carriage_return() {
        let findings = vec![
            Finding::new(
                Level::Warning,
                Check::EnvLocalHostPort,
                "a\u{1b}[2K\rb\u{7}",
            )
            .in_file(".env\u{1b}]52;c;x")
            .for_key("K\u{1b}[8m"),
        ];

        let text = render_text(&report(vec![], findings), plain());

        assert!(
            text.chars()
                .all(|character| !character.is_control() || character == '\n')
        );
        assert!(text.contains("a\\u{1b}[2K\\rb\\u{7}"));
    }

    fn service_port() -> ManagedPort {
        ManagedPort {
            variable: "DB_PORT".to_owned(),
            kind: PortKind::Service,
            default: Some(5432),
            source: PortSource::Compose,
            file: "dev.compose.yml".to_owned(),
            service: Some("postgres".to_owned()),
            target: Some(5432),
        }
    }

    #[test]
    fn should_render_the_whole_report_when_there_are_ports_and_problems() {
        let findings = vec![
            Finding::new(Level::Error, Check::ContainerName, "remove it")
                .in_file("compose.yml")
                .in_service("app"),
            Finding::new(Level::Warning, Check::ViteHmrPort, "add the line")
                .in_file("vite.config.ts"),
        ];

        let text = render_text(&report(vec![service_port()], findings), plain());

        assert_eq!(
            text,
            "Checkout:      /work/app\n\
             Compose file:  dev.compose.yml\n\
             \n\
             Ports\n\
             \x20 VARIABLE  SOURCE           DEFAULT  SERVICE\n\
             \x20 DB_PORT   dev.compose.yml  5432     postgres:5432\n\
             \n\
             Problems\n\
             \x20 error   compose.yml (app): remove it\n\
             \x20 warning vite.config.ts: add the line\n\
             \n\
             1 error, 1 warning\n"
        );
    }

    #[test]
    fn should_say_none_when_there_is_nothing_to_list() {
        let mut empty = report(vec![], vec![]);
        empty.compose_file = None;

        let text = render_text(&empty, plain());

        assert!(text.contains("Compose file:  none\n"));
        assert!(text.contains("Ports\n  none found\n"));
        assert!(text.ends_with("Problems\n  none\n\n0 errors, 0 warnings\n"));
    }

    fn registered_info() -> RegisteredInfo {
        RegisteredInfo {
            version: 1,
            registered: true,
            name: "app-3f2a".to_owned(),
            path: "/work/app".to_owned(),
            slot: 1,
            host: "app-3f2a.localhost".to_owned(),
            app_url: None,
            range: [20050, 20099],
            compose_file: Some("dev.compose.yml".to_owned()),
            compose_file_origin: Some("detected".to_owned()),
            ports: vec![
                crate::report::InfoPort {
                    variable: "PORT".to_owned(),
                    port: 20050,
                    kind: PortKind::Web,
                    service: None,
                    target: None,
                    url: Some("http://localhost:20050".to_owned()),
                    forced: false,
                },
                crate::report::InfoPort {
                    variable: "DB_PORT".to_owned(),
                    port: 20052,
                    kind: PortKind::Service,
                    service: Some("postgres".to_owned()),
                    target: Some(5432),
                    url: None,
                    forced: false,
                },
            ],
            services: ServicesState::Running,
        }
    }

    #[test]
    fn should_render_the_registered_info_when_the_instance_has_ports() {
        let text = render_info_text(&InfoReport::Registered(registered_info()));

        assert_eq!(
            text,
            "Name:          app-3f2a\n\
             Path:          /work/app\n\
             Slot:          1 (20050-20099)\n\
             Host:          app-3f2a.localhost\n\
             Compose file:  dev.compose.yml (detected)\n\
             Services:      running\n\
             \n\
             Ports\n\
             \x20 VARIABLE  PORT   KIND     SERVICE        URL\n\
             \x20 PORT      20050  web      -              http://localhost:20050\n\
             \x20 DB_PORT   20052  service  postgres:5432  -\n"
        );
    }

    #[test]
    fn should_mark_a_forced_port_in_words_when_rendering_the_ports() {
        let mut info = registered_info();
        info.ports[0].port = 3333;
        info.ports[0].forced = true;

        let text = render_info_text(&InfoReport::Registered(info));

        assert!(
            text.contains("PORT      3333 (forced in devslot.local.toml)"),
            "{text}"
        );
        assert!(!text.contains("20052 (forced"));
    }

    #[test]
    fn should_render_the_app_url_after_the_host_when_the_instance_has_one() {
        let mut info = registered_info();
        info.app_url = Some("http://app-3f2a.localhost:20050".to_owned());

        let text = render_info_text(&InfoReport::Registered(info));

        assert!(text.contains(
            "Host:          app-3f2a.localhost\nApp URL:       http://app-3f2a.localhost:20050\nCompose file:"
        ));
    }

    #[test]
    fn should_say_how_to_check_the_services_when_they_were_not_asked() {
        let text = render_registered_info_unchecked(&registered_info());

        assert!(text.contains("Services:      run ds info to check\n"));
        assert!(!text.contains("Services:      running"));
    }

    fn listed(name: &str, services_running: Option<bool>) -> crate::report::ListedInstance {
        crate::report::ListedInstance {
            name: name.to_owned(),
            path: format!("/work/{name}"),
            slot: 1,
            host: format!("{name}.localhost"),
            services_running,
            ports: registered_info().ports,
        }
    }

    #[test]
    fn should_render_the_table_the_ports_and_the_opted_out_paths_when_listing() {
        let report = ListReport {
            version: 1,
            shared: Vec::new(),
            instances: vec![listed("app-3f2a", Some(true)), listed("web-9b1c", None)],
        };

        let text = render_list_text(&report, &["/work/old".to_owned()]);

        assert!(text.starts_with(
            "  NAME      SLOT  SERVICES  PATH\n\
             \x20 app-3f2a  1     running   /work/app-3f2a\n\
             \x20 web-9b1c  1     unknown   /work/web-9b1c\n\
             \nPorts of app-3f2a\n"
        ));
        assert!(text.contains("\nPorts of web-9b1c\n"));
        assert!(text.ends_with("\nOpted out\n  /work/old\n"));
    }

    #[test]
    fn should_align_the_columns_when_a_cell_has_non_ascii_characters() {
        let header = ["PATH", "SLOT"].map(str::to_owned);
        let rows = vec![
            ["/home/me/dév".to_owned(), "1".to_owned()],
            ["/home/me/dev".to_owned(), "2".to_owned()],
        ];

        assert_eq!(
            render_table(header, rows),
            "  PATH          SLOT\n\
             \x20 /home/me/dév  1\n\
             \x20 /home/me/dev  2\n"
        );
    }

    #[test]
    fn should_list_the_kept_worktrees_with_the_command_that_removes_them() {
        let kept = vec![KeptWorktree {
            path: "/wt/app/feat".into(),
            reason: "in use by 12 (node)".to_owned(),
            date: "2026-11-16".to_owned(),
        }];

        assert_eq!(
            render_kept_worktrees(&kept),
            "\nWorktrees Claude Code asked to remove that are still there:\n\
             \x20 /wt/app/feat: in use by 12 (node) (2026-11-16); remove with ds worktree rm '/wt/app/feat'\n"
        );
    }

    #[test]
    fn should_print_nothing_when_no_worktree_was_kept() {
        assert_eq!(render_kept_worktrees(&[]), "");
    }

    #[test]
    fn should_say_that_nothing_is_registered_when_listing_an_empty_machine() {
        let report = ListReport {
            version: 1,
            shared: Vec::new(),
            instances: Vec::new(),
        };

        assert_eq!(
            render_list_text(&report, &[]),
            "No instance is registered on this machine.\n"
        );
    }

    #[test]
    fn should_say_not_running_when_docker_reports_no_container_of_the_instance() {
        let report = ListReport {
            version: 1,
            shared: Vec::new(),
            instances: vec![listed("app-3f2a", Some(false))],
        };

        assert!(render_list_text(&report, &[]).contains("not running"));
    }

    #[test]
    fn should_say_none_when_the_instance_has_no_compose_file_and_no_port() {
        let mut info = registered_info();
        info.compose_file = None;
        info.compose_file_origin = None;
        info.ports.clear();

        let text = render_info_text(&InfoReport::Registered(info));

        assert!(text.contains("Compose file:  none\n"));
        assert!(text.ends_with("Ports\n  none\n"));
    }

    #[test]
    fn should_suggest_ds_up_when_the_checkout_is_not_registered() {
        let text = render_info_text(&InfoReport::Unregistered(crate::report::UnregisteredInfo {
            version: 1,
            registered: false,
            path: "/work/app".to_owned(),
            opted_out: false,
        }));

        assert_eq!(
            text,
            "Path:          /work/app\nThis checkout is not registered. Run ds up to register it.\n"
        );
    }

    #[test]
    fn should_say_the_checkout_is_opted_out_when_ds_rm_opted_it_out() {
        let text = render_info_text(&InfoReport::Unregistered(crate::report::UnregisteredInfo {
            version: 1,
            registered: false,
            path: "/work/app".to_owned(),
            opted_out: true,
        }));

        assert!(text.contains("opted out by ds rm; ds up or ds run registers it again"));
    }

    #[test]
    fn should_keep_the_level_words_when_color_is_off() {
        let findings = vec![Finding::new(
            Level::Info,
            Check::ComposeMissing,
            "no compose file",
        )];

        let text = render_text(&report(vec![], findings), plain());

        assert!(text.contains("  info    no compose file\n"));
    }

    #[test]
    fn should_color_only_the_level_word_when_color_is_on() {
        let findings = vec![Finding::new(Level::Error, Check::ContainerName, "m")];

        let text = render_text(&report(vec![], findings), Style { color: true });

        assert!(text.contains("\x1b[31merror  \x1b[0m m\n"));
    }
}
