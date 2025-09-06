use tigris_security::idguard::{
    parse_pattern, sanitize_cfg, IdgConfig, IdgThresholds, IdgWeights, RuleKind,
};

#[test]
fn parse_pattern_distinguishes_token_and_regex() {
    // plain token
    assert_eq!(
        parse_pattern("foo"),
        (RuleKind::Token, "foo".to_string())
    );

    // token with leading slash but no closing slash
    assert_eq!(
        parse_pattern("/foo"),
        (RuleKind::Token, "/foo".to_string())
    );

    // classic regex /body/
    assert_eq!(
        parse_pattern("/foo/"),
        (RuleKind::Regex, "/foo/".to_string())
    );

    // regex with flags /body/flags
    assert_eq!(
        parse_pattern("/foo/i"),
        (RuleKind::Regex, "/foo/i".to_string())
    );
}

#[test]
fn sanitize_cfg_clamps_thresholds() {
    let cfg = IdgConfig {
        thresholds: IdgThresholds { watch: 200, block: 0 },
        ..Default::default()
    };
    let cfg = sanitize_cfg(cfg);
    assert_eq!(cfg.thresholds.block, 1);
    assert_eq!(cfg.thresholds.watch, 0);

    let cfg = IdgConfig {
        thresholds: IdgThresholds { watch: 250, block: 150 },
        ..Default::default()
    };
    let cfg = sanitize_cfg(cfg);
    assert_eq!(cfg.thresholds.block, 100);
    assert_eq!(cfg.thresholds.watch, 99);
}

#[test]
fn sanitize_cfg_clamps_weights() {
    let cfg = IdgConfig {
        weights: IdgWeights {
            nick_token: -5,
            nick_regex: 150,
            avatar_hash: 50,
            avatar_ocr: 101,
            avatar_nsfw: -1,
        },
        ..Default::default()
    };
    let cfg = sanitize_cfg(cfg);
    assert_eq!(cfg.weights.nick_token, 0);
    assert_eq!(cfg.weights.nick_regex, 100);
    assert_eq!(cfg.weights.avatar_hash, 50);
    assert_eq!(cfg.weights.avatar_ocr, 100);
    assert_eq!(cfg.weights.avatar_nsfw, 0);
}