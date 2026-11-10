//! The text output of `ds doctor`.

use std::fmt::Write;

use super::style::{Style, Tone};
use crate::checks::finding::{Finding, Level};
use crate::discovery::ManagedPort;
use crate::report::DoctorReport;

const LEVEL_WORD_WIDTH: usize = 7;
const NO_VALUE: &str = "-";

/// Renders the report as the text `ds doctor` prints.
pub fn render_text(report: &DoctorReport, style: Style) -> String {
    let mut output = String::new();
    let compose_file = report.compose_file.as_deref().unwrap_or("none");
    let _ = writeln!(output, "Checkout:      {}", report.checkout);
    let _ = writeln!(output, "Compose file:  {compose_file}");
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
    [port.variable.clone(), port.file.clone(), default, target]
}

fn render_ports(ports: &[ManagedPort]) -> String {
    if ports.is_empty() {
        return "  none found\n".to_owned();
    }
    let header = ["VARIABLE", "SOURCE", "DEFAULT", "SERVICE"].map(str::to_owned);
    let rows: Vec<[String; 4]> = std::iter::once(header)
        .chain(ports.iter().map(port_row))
        .collect();
    let widths: [usize; 4] =
        std::array::from_fn(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0));
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

fn render_findings(findings: &[Finding], style: Style) -> String {
    if findings.is_empty() {
        return "  none\n".to_owned();
    }
    let mut output = String::new();
    for finding in findings {
        let _ = writeln!(output, "  {}", render_finding(finding, style));
    }
    output
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
    format!("{} {location}{}", style.paint(tone, &word), finding.message)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::checks::finding::{Check, Summary};
    use crate::discovery::{PortKind, PortSource};

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
