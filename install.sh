#!/usr/bin/env bash
set -euo pipefail

repo="${REMARKABLE_MCP_REPO:-Bradley-Butcher/remarkable-mcp}"
install_dir="${REMARKABLE_MCP_INSTALL_DIR:-${HOME}/.local/bin}"
version="${1:-latest}"

case "$(uname -s)" in
  Linux) os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *)
    echo "Unsupported OS. Download a release binary from https://github.com/${repo}/releases" >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *)
    echo "Unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

target="${arch}-${os}"
archive="remarkable-mcp-${target}.tar.gz"
if [[ "${version}" == "latest" ]]; then
  base_url="https://github.com/${repo}/releases/latest/download"
else
  version="${version#v}"
  base_url="https://github.com/${repo}/releases/download/v${version}"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

download() {
  local url="$1"
  local output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "${url}" --output "${output}"
  elif command -v wget >/dev/null 2>&1; then
    wget --quiet "${url}" --output-document="${output}"
  else
    echo "Install curl or wget and try again." >&2
    exit 1
  fi
}

download "${base_url}/${archive}" "${tmp_dir}/${archive}"
download "${base_url}/${archive}.sha256" "${tmp_dir}/${archive}.sha256"

cd "${tmp_dir}"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum --check "${archive}.sha256"
elif command -v shasum >/dev/null 2>&1; then
  shasum --algorithm 256 --check "${archive}.sha256"
else
  echo "No SHA-256 utility found; refusing an unverified install." >&2
  exit 1
fi

tar -xzf "${archive}"
mkdir -p "${install_dir}"
install -m 0755 remarkable-mcp "${install_dir}/remarkable-mcp"

echo "Installed remarkable-mcp to ${install_dir}/remarkable-mcp"
if [[ ":${PATH}:" != *":${install_dir}:"* ]]; then
  echo "Add ${install_dir} to PATH before configuring your MCP client."
fi
