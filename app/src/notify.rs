//! Optional SMTP alert, sent off-thread so a slow server never stalls the watchdog.

use crate::config::Email;
use crate::log::Log;
use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use std::path::PathBuf;

pub fn send(cfg: &Email, subject: String, body: String, attachments: Vec<PathBuf>, log: Log) {
    if !cfg.enabled || cfg.to.is_empty() || cfg.smtp_host.is_empty() {
        return;
    }
    let cfg = cfg.clone();
    std::thread::spawn(move || match build_and_send(&cfg, &subject, &body, &attachments) {
        Ok(()) => log.line(&format!("email sent: {subject}")),
        Err(e) => log.line(&format!("email failed: {e}")),
    });
}

fn build_and_send(cfg: &Email, subject: &str, body: &str, attachments: &[PathBuf]) -> Result<(), String> {
    let from = if cfg.from.is_empty() { &cfg.username } else { &cfg.from };
    let mut builder = Message::builder().from(from.parse().map_err(|e| format!("from: {e}"))?).subject(subject);
    for to in &cfg.to {
        builder = builder.to(to.parse().map_err(|e| format!("to {to}: {e}"))?);
    }
    let mut parts = MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()));
    if cfg.attach {
        for path in attachments {
            if let Ok(bytes) = std::fs::read(path) {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let ct = if name.ends_with(".png") { "image/png" } else { "text/plain" };
                parts = parts.singlepart(Attachment::new(name).body(bytes, ContentType::parse(ct).unwrap()));
            }
        }
    }
    let msg = builder.multipart(parts).map_err(|e| e.to_string())?;

    let password = std::env::var(&cfg.password_env).ok().filter(|p| !p.is_empty()).unwrap_or_else(|| cfg.password.clone());
    let transport = match cfg.security.as_str() {
        "tls" => SmtpTransport::relay(&cfg.smtp_host),
        "none" => Ok(SmtpTransport::builder_dangerous(&cfg.smtp_host)),
        _ => SmtpTransport::starttls_relay(&cfg.smtp_host),
    }
    .map_err(|e| e.to_string())?
    .port(cfg.smtp_port);
    let transport = if cfg.username.is_empty() { transport } else { transport.credentials(Credentials::new(cfg.username.clone(), password)) };
    transport.build().send(&msg).map(|_| ()).map_err(|e| e.to_string())
}
