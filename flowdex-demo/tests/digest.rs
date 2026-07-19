use flowdex_demo::parser::parse_incident;
use flowdex_demo::render::render_digest;
use flowdex_demo::{Incident, Severity};

#[test]
fn parse_accepts_the_documented_format() {
    assert_eq!(
        parse_incident("critical|Database unavailable|on-call"),
        Ok(Incident {
            title: "Database unavailable".to_string(),
            severity: Severity::Critical,
            owner: "on-call".to_string(),
        })
    );
}

#[test]
fn parse_rejects_unknown_severity() {
    let error = parse_incident("urgent|Database unavailable|on-call").unwrap_err();
    assert!(error.contains("unknown severity"));
}

#[test]
fn render_orders_incidents_and_formats_a_digest() {
    let incidents = vec![
        Incident {
            title: "Update dashboard".to_string(),
            severity: Severity::Low,
            owner: "maya".to_string(),
        },
        Incident {
            title: "Database unavailable".to_string(),
            severity: Severity::Critical,
            owner: "on-call".to_string(),
        },
    ];

    assert_eq!(
        render_digest(&incidents),
        "# Incident digest\n\n- [CRITICAL] Database unavailable (@on-call)\n- [LOW] Update dashboard (@maya)\n"
    );
}
