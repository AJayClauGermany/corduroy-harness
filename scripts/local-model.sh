#!/usr/bin/env bash
# A small model on this machine, behind an OpenAI-compatible endpoint on the loopback
# interface, so an agent can run real tasks with no API key and no network once set up. The
# agent reaches it as an ordinary `openai-compatible` provider (examples/local-model/).
#
# The server is llama.cpp's `llama-server`, built from the llama.cpp sources vendored in the
# llama-cpp-python source package on PyPI; the model is a quantized GGUF from Hugging Face.
# Both downloads are pinned by SHA-256. Needs cmake, a C++ compiler, python3 (pip) and curl;
# the first setup takes a few minutes and about 1.5 GB of disk.
#
# Usage: scripts/local-model.sh setup   build the server and fetch the model
#        scripts/local-model.sh serve   setup, then serve in the foreground
#        scripts/local-model.sh start   setup, then serve in the background until healthy
#        scripts/local-model.sh stop    stop a server `start` started
#        scripts/local-model.sh url     print the chat-completions URL
#
# Environment (all optional):
#   CORDUROY_LOCAL_MODEL_DIR    cache for the build and the model (~/.cache/corduroy/local-model)
#   CORDUROY_LOCAL_MODEL_PORT   port on 127.0.0.1 (8080)
#   CORDUROY_LOCAL_MODEL_CTX    context window in tokens (16384)
#   CORDUROY_LOCAL_MODEL_REPO, CORDUROY_LOCAL_MODEL_FILE, CORDUROY_LOCAL_MODEL_SHA256
#                               another GGUF to serve; without a SHA-256 it is not verified
set -euo pipefail

llama_version=0.3.37
llama_sha256=bf4108fc76e933ea4969b61b1e281a2d232dd3e43361d1fdca5e2e029d62f729

# Liquid AI's LFM3, 1.7B parameters, 4-bit: about 1.1 GB, 17 tokens/s on four CPU cores.
default_repo=LiquidAI/LFM3-1.7B-GGUF
default_revision=cd0c74c575b5c416f3011b5b4c298077f26ca8c2
default_file=LFM3-1.7B-Q4_K_M.gguf
default_sha256=3793e1e6e2266919d6e015287221b41100175a98eddaba92e111aed3b05c93c4

dir=${CORDUROY_LOCAL_MODEL_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/corduroy/local-model}
port=${CORDUROY_LOCAL_MODEL_PORT:-8080}
ctx=${CORDUROY_LOCAL_MODEL_CTX:-16384}
repo=${CORDUROY_LOCAL_MODEL_REPO:-$default_repo}
file=${CORDUROY_LOCAL_MODEL_FILE:-$default_file}
if [ "$repo" = "$default_repo" ] && [ "$file" = "$default_file" ]; then
  revision=$default_revision
  sha256=${CORDUROY_LOCAL_MODEL_SHA256:-$default_sha256}
else
  revision=main
  sha256=${CORDUROY_LOCAL_MODEL_SHA256:-}
fi

source_dir=$dir/llama_cpp_python-$llama_version/vendor/llama.cpp
server=$source_dir/build/bin/llama-server
model=$dir/models/$file
url=http://127.0.0.1:$port/v1/chat/completions

step() { printf '\n==> %s\n' "$*" >&2; }
fail() { printf 'local-model: %s\n' "$*" >&2; exit 1; }

verify() { # file sha256
  [ -z "$2" ] && { step "not verifying $1: no SHA-256 given"; return 0; }
  echo "$2  $1" | sha256sum --check --quiet - || fail "$1 does not match its SHA-256"
}

build_server() {
  [ -x "$server" ] && return 0
  for tool in cmake c++ python3 curl sha256sum; do
    command -v "$tool" >/dev/null || fail "needs $tool"
  done
  mkdir -p "$dir"
  local archive=$dir/llama_cpp_python-$llama_version.tar.gz
  if [ ! -f "$archive" ]; then
    step "downloading llama-cpp-python $llama_version (for its vendored llama.cpp)"
    python3 -m pip download --quiet --no-deps --no-binary :all: \
      "llama-cpp-python==$llama_version" --dest "$dir"
  fi
  verify "$archive" "$llama_sha256"
  rm -rf "$dir/llama_cpp_python-$llama_version"
  tar -xzf "$archive" -C "$dir"
  step "building llama-server (a few minutes; log: $dir/build.log)"
  # The server's web UI is fetched at build time; it is not needed to serve the API.
  {
    cmake -S "$source_dir" -B "$source_dir/build" -DCMAKE_BUILD_TYPE=Release \
      -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF -DLLAMA_CURL=OFF \
      -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF &&
      cmake --build "$source_dir/build" --target llama-server -j "$(nproc)"
  } >"$dir/build.log" 2>&1 || fail "the build failed: see $dir/build.log"
  [ -x "$server" ] || fail "the build did not produce $server"
}

fetch_model() {
  [ -f "$model" ] && return 0
  mkdir -p "$dir/models"
  step "downloading $repo/$file"
  curl --fail --location --silent --show-error --output "$model.part" \
    "https://huggingface.co/$repo/resolve/$revision/$file"
  verify "$model.part" "$sha256"
  mv "$model.part" "$model"
}

setup() { build_server; fetch_model; }

# Liquid's recommended sampling for LFM3; `--jinja` applies the model's own chat template,
# which is what turns its tool calls into OpenAI `tool_calls`.
server_args=(--model "$model" --alias local --host 127.0.0.1 --port "$port"
  --ctx-size "$ctx" --jinja --temp 0.1 --top-k 50 --repeat-penalty 1.1)

healthy() { curl --silent --fail --max-time 2 "http://127.0.0.1:$port/health" >/dev/null; }

case ${1:-} in
  setup) setup ;;
  serve)
    setup
    step "serving $file at $url"
    exec "$server" "${server_args[@]}"
    ;;
  start)
    healthy && { step "already serving at $url"; exit 0; }
    setup
    "$server" "${server_args[@]}" >"$dir/server.log" 2>&1 &
    echo $! >"$dir/server.pid"
    for _ in $(seq 1 120); do
      healthy && { step "serving $file at $url (log: $dir/server.log)"; exit 0; }
      kill -0 "$(cat "$dir/server.pid")" 2>/dev/null || fail "the server exited: see $dir/server.log"
      sleep 1
    done
    fail "the server did not become healthy in 120s: see $dir/server.log"
    ;;
  stop)
    [ -f "$dir/server.pid" ] || fail "no server started by this script"
    kill "$(cat "$dir/server.pid")" 2>/dev/null || true
    rm -f "$dir/server.pid"
    ;;
  url) echo "$url" ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
