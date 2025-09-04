#!/usr/bin/env bash
set -euo pipefail
export RUST_LOG=${RUST_LOG:-tigrissystem_security=info,sqlx=warn}
export TSS_ENV=${TSS_ENV:-development}


# Załaduj odpowiedni .env
if [ -f .env.$TSS_ENV ]; then
set -a; source .env.$TSS_ENV; set +a
fi


cargo run --bin tss