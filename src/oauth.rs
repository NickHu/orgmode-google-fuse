use std::sync::LazyLock;

use google_tasks1::yup_oauth2::ApplicationSecret;

pub(crate) static APPLICATION_SECRET: LazyLock<ApplicationSecret> =
    LazyLock::new(|| ApplicationSecret {
        client_id: "550516298540-e6crakmca8u91amaf1h25gih29ja0q7n.apps.googleusercontent.com"
            .to_owned(),
        client_secret: "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".to_owned(),
        token_uri: "https://oauth2.googleapis.com/token".to_owned(),
        auth_uri: "https://accounts.google.com/o/oauth2/auth".to_owned(),
        project_id: Some("orgmode-g-fuse".to_owned()),
        auth_provider_x509_cert_url: Some("https://www.googleapis.com/oauth2/v1/certs".to_owned()),
        redirect_uris: vec!["urn:ietf:wg:oauth:2.0:oob", "oob"]
            .into_iter()
            .map(|s| s.to_owned())
            .collect(),
        ..ApplicationSecret::default()
    });
