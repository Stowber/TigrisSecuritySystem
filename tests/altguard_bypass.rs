use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::postgres::PgPoolOptions;
use tigris_security::altguard::{
    AltGuard, test_fetch_and_ahash_inner, test_is_trusted_discord_cdn,
    TEST_MAX_IMAGE_BYTES, TestMessageFP, test_weight_behavior_pattern,
};
use tigris_security::config::{App, Database, Discord, Logging, Settings};
use tigris_security::AppContext;

fn make_altguard() -> Arc<AltGuard> {
    let settings = Settings {
        env: "test".into(),
        app: App { name: "test".into() },
        discord: Discord { token: String::new(), app_id: None, intents: vec![] },
        database: Database { url: "postgres://localhost:1/test?connect_timeout=1".into(), max_connections: Some(1), statement_timeout_ms: Some(5_000) },
        logging: Logging { json: Some(false), level: Some("info".into()) },
    };
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&settings.database.url)
        .unwrap();
    let ctx = AppContext::new_testing(settings, db);
    AltGuard::new(ctx)
}

fn confusable_variants(name: &str) -> Vec<String> {
    let mapping = [
        ('e', 'е'), // Cyrillic e
        ('t', 'т'),
        ('s', 'ѕ'),
        ('u', 'υ'),
    ];
    let base: Vec<char> = name.chars().collect();
    let mut variants = Vec::new();
    for &(latin, conf) in &mapping {
        for (i, ch) in base.iter().enumerate() {
            if *ch == latin {
                let mut new_chars = base.clone();
                new_chars[i] = conf;
                variants.push(new_chars.iter().collect());
            }
        }
    }
    variants
}

#[tokio::test]
async fn detects_confusable_names() {
    let ag = make_altguard();
    ag.push_punished_name(1, "testuser").await;

    for variant in confusable_variants("testuser") {
        let weight = ag
            .test_similarity_to_punished(1, &[variant.clone()])
            .await
            .unwrap()
            .unwrap_or(0);
        assert!(weight > 0, "{} not flagged", variant);
    }
}

#[test]
fn rejects_untrusted_avatar_url() {
    assert!(!test_is_trusted_discord_cdn("http://evil.com/avatar.png"));
    assert!(test_is_trusted_discord_cdn("https://cdn.discordapp.com/avatars/0.png"));
}

#[tokio::test]
async fn rejects_large_avatars() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = vec![0u8; TEST_MAX_IMAGE_BYTES + 1];
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
    });

    let url = format!("http://{}/big.png", addr);
    let hash = test_fetch_and_ahash_inner(&url).await.unwrap();
    assert!(hash.is_none());
    server.await.unwrap();
}

#[test]
fn detects_delayed_spam_after_join() {
    let join_at = Instant::now();
    let mut msgs = Vec::new();
    for i in 0..5 {
        msgs.push(TestMessageFP {
            at: join_at + Duration::from_secs(60 + i),
            has_link: i == 0,
            mentions: 5,
            len: 3,
            sig: 1,
            repeated_special: true,
            entropy: 1.0,
        });
    }
    let weight = test_weight_behavior_pattern(&msgs, 15);
    assert!(weight > 0);
}