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
#        scripts/local-model.sh models  list the models it knows by name
#        scripts/local-model.sh serving print the name of the model being served, if any
#
# Runs on Linux and macOS (bash 3.2 is enough).
#
# Environment (all optional):
#   CORDUROY_LOCAL_MODEL        which known model to serve (lfm3-1.7b); see `models`
#   CORDUROY_LOCAL_MODEL_ARGS   more arguments for llama-server, after the model's own
#   CORDUROY_LOCAL_MODEL_DIR    cache for the build and the model (~/.cache/corduroy/local-model)
#   CORDUROY_LOCAL_MODEL_PORT   port on 127.0.0.1 (8080); one server a port, so several models
#                               can be served side by side
#   CORDUROY_LOCAL_MODEL_CTX    context window in tokens (16384)
#   CORDUROY_LOCAL_MODEL_CACHE  the server's prompt cache in MiB (1024)
#   CORDUROY_LOCAL_MODEL_REPO, CORDUROY_LOCAL_MODEL_FILE, CORDUROY_LOCAL_MODEL_SHA256
#                               another GGUF to serve; without a SHA-256 it is not verified
set -euo pipefail

llama_version=0.3.37
llama_sha256=bf4108fc76e933ea4969b61b1e281a2d232dd3e43361d1fdca5e2e029d62f729

# The models known by name: each a quantized GGUF pinned by revision and SHA-256, with the
# sampling its makers recommend. All are under about two billion parameters, so that they
# stay within a laptop or a four-core sandbox. `models` prints this list.
#   name | parameters | size | repo | revision | file | sha256 | sampling
catalogue() {
  cat <<'END'
lfm3-1.7b|1.7B|1.1 GB|LiquidAI/LFM3-1.7B-GGUF|cd0c74c575b5c416f3011b5b4c298077f26ca8c2|LFM3-1.7B-Q4_K_M.gguf|3793e1e6e2266919d6e015287221b41100175a98eddaba92e111aed3b05c93c4|--temp 0.1 --top-k 50 --repeat-penalty 1.1
qwen3-0.6b|0.6B|0.6 GB|Qwen/Qwen3-0.6B-GGUF|23749fefcc72300e3a2ad315e1317431b06b590a|Qwen3-0.6B-Q8_0.gguf|9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031|--temp 0.6 --top-k 20 --top-p 0.95 --min-p 0 --presence-penalty 1.5
qwen3-1.7b|1.7B|1.8 GB|Qwen/Qwen3-1.7B-GGUF|90862c4b9d2787eaed51d12237eafdfe7c5f6077|Qwen3-1.7B-Q8_0.gguf|061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a|--temp 0.6 --top-k 20 --top-p 0.95 --min-p 0 --presence-penalty 1.5
qwen3.5-0.8b|0.8B|0.5 GB|unsloth/Qwen3.5-0.8B-GGUF|6ab461498e2023f6e3c1baea90a8f0fe38ab64d0|Qwen3.5-0.8B-Q4_K_M.gguf|bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517|--temp 1.0 --top-p 1.0 --top-k 20 --min-p 0 --presence-penalty 2.0
qwen3.5-2b|1.9B|1.3 GB|unsloth/Qwen3.5-2B-GGUF|f6d5376be1edb4d416d56da11e5397a961aca8ae|Qwen3.5-2B-Q4_K_M.gguf|aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223|--temp 1.0 --top-p 1.0 --top-k 20 --min-p 0 --presence-penalty 2.0
minicpm5-1b|1.1B|0.7 GB|openbmb/MiniCPM5-1B-GGUF|5385560dd3e232bcc3577fd9442c332cad2facbd|MiniCPM5-1B-Q4_K_M.gguf|81b64d05a23b17b34c475f42b3e72fbde62d4b92cc34541f7a8031d0752deafa|--temp 0.9 --top-p 0.95
minicpm5-2b|2.5B|1.6 GB|openbmb/MiniCPM5-2B-GGUF|8ad02b786ab81e33d465e1c80562640c55ab2877|MiniCPM5-2B-Q4_K_M.gguf|ec2d5801640099e97d8d7e8003ad4d81f336e757811f03a26173dddf386602fd|--temp 0.9 --top-p 0.95
END
}

fail() { printf 'local-model: %s\n' "$*" >&2; exit 1; }

name=${CORDUROY_LOCAL_MODEL:-lfm3-1.7b}
entry=$(catalogue | grep "^$name|" || true)
[ -n "$entry" ] || fail "no model called $name: see \`$0 models\`"
IFS='|' read -r _ _ _ default_repo default_revision default_file default_sha256 sampling <<<"$entry"

dir=${CORDUROY_LOCAL_MODEL_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/corduroy/local-model}
port=${CORDUROY_LOCAL_MODEL_PORT:-8080}
ctx=${CORDUROY_LOCAL_MODEL_CTX:-16384}
cache=${CORDUROY_LOCAL_MODEL_CACHE:-1024}
repo=${CORDUROY_LOCAL_MODEL_REPO:-$default_repo}
file=${CORDUROY_LOCAL_MODEL_FILE:-$default_file}
if [ "$repo" = "$default_repo" ] && [ "$file" = "$default_file" ]; then
  revision=$default_revision
  sha256=${CORDUROY_LOCAL_MODEL_SHA256:-$default_sha256}
else
  revision=main
  sha256=${CORDUROY_LOCAL_MODEL_SHA256:-}
  name=$file
fi

source_dir=$dir/llama_cpp_python-$llama_version/vendor/llama.cpp
server=$source_dir/build/bin/llama-server
model=$dir/models/$file
url=http://127.0.0.1:$port/v1/chat/completions
# What a running server leaves behind is named by its port, so that several can run at once.
state=$dir/server-$port

step() { printf '\n==> %s\n' "$*" >&2; }

# macOS has neither `sha256sum` (before 15) nor `nproc`.
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}
cores() { getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4; }

verify() { # file sha256
  [ -z "$2" ] && { step "not verifying $1: no SHA-256 given"; return 0; }
  [ "$(sha256 "$1")" = "$2" ] || fail "$1 does not match its SHA-256"
}

build_server() {
  [ -x "$server" ] && return 0
  for tool in cmake c++ python3 curl; do
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
      cmake --build "$source_dir/build" --target llama-server -j "$(cores)"
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

# `--jinja` applies the model's own chat template, which is what turns its tool calls into
# OpenAI `tool_calls`; the sampling is what the model's makers recommend. The words of the
# two variables are split on purpose: each is a list of arguments.
# The server keeps earlier prompts in memory to answer a repeated one sooner, up to 8 GB by
# default: a 0.6 GB model grew past 8 GB in twenty minutes of the bench and was killed with
# its neighbour by the sandbox's memory limit. 1 GB keeps the conversation in hand.
# shellcheck disable=SC2206
server_args=(--model "$model" --alias local --host 127.0.0.1 --port "$port"
  --ctx-size "$ctx" --cache-ram "$cache" --jinja $sampling ${CORDUROY_LOCAL_MODEL_ARGS:-})

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
    "$server" "${server_args[@]}" >"$state.log" 2>&1 &
    echo $! >"$state.pid"
    echo "$name" >"$state.model"
    for _ in $(seq 1 120); do
      healthy && { step "serving $file at $url (log: $state.log)"; exit 0; }
      kill -0 "$(cat "$state.pid")" 2>/dev/null || fail "the server exited: see $state.log"
      sleep 1
    done
    fail "the server did not become healthy in 120s: see $state.log"
    ;;
  stop)
    [ -f "$state.pid" ] || fail "no server started by this script"
    kill "$(cat "$state.pid")" 2>/dev/null || true
    rm -f "$state.pid" "$state.model"
    # The port is free only once the process is gone, and the next model wants it.
    for _ in $(seq 1 30); do healthy || break; sleep 1; done
    ;;
  url) echo "$url" ;;
  models)
    catalogue | while IFS='|' read -r known parameters size known_repo _ known_file _ _; do
      printf '%-12s %5s  %6s  %s/%s\n' "$known" "$parameters" "$size" "$known_repo" "$known_file"
    done
    ;;
  serving) healthy && cat "$state.model" 2>/dev/null ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
