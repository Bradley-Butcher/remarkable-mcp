# remarkable-mcp

Use your reMarkable library from an AI assistant.

Ask it to find a notebook, show a page, upload a PDF, or tidy your folders. `remarkable-mcp` connects directly to reMarkable Cloud and keeps every response small enough to be useful model context.

> Cloud only, as it is the only reMarkable workflow I use.

## Design intent

The primary goal is to pass reMarkable pages as image payloads to coding agents that support vision. OCR is ultimately a lossy compression of a page: it can discard handwriting character, layout, diagrams, and the relationship between annotations and source material. Keeping the page visual preserves that information for the model.

Token efficiency matters too. Reads return one bounded image at a time, discovery tools return short paginated results, and management tools answer with a single concise confirmation wherever possible.

## Quick start

### 1. Install

Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/Bradley-Butcher/remarkable-mcp/main/install.sh | bash
```

This installs the latest verified release to `~/.local/bin`. Windows users can download the `.zip` from [GitHub Releases](https://github.com/Bradley-Butcher/remarkable-mcp/releases).

If `remarkable-mcp` is not found after installation, add the install directory to your `PATH` and restart your MCP client.

### 2. Add it to your MCP client

Use `remarkable-mcp` as a local stdio server. The common configuration shape is:

```json
{
  "mcpServers": {
    "remarkable": {
      "command": "remarkable-mcp"
    }
  }
}
```

Your client may use a different outer key or settings screen, but the command stays the same.

### 3. Connect your account

Restart your MCP client, then tell your assistant:

> Connect to my reMarkable.

The official reMarkable connection page opens in your browser. Paste the one-time code back into the conversation when asked. The server exchanges it for a device token and stores that token locally; the code and token are never returned to the model.

You can now ask:

> Show me my most recent note.

## Things to try

```text
Show my 10 most recent reMarkable documents.

Browse the /Work/Meetings folder.

Search my reMarkable for "launch plan".

Show page 3 of /Work/Meetings/Launch review.

Zoom in on the bottom-right quarter of that page.

Upload ./papers/attention.pdf to /Reading.

Create /Work/Archive and move the old planning notes there.

Move /Scratch/Unused to trash.
```

Full paths are best when two documents share a name. Page numbers are one-based.

## What the assistant can do

| Tool | What it does |
|---|---|
| `remarkable_connect` | Opens the official connection page or finishes connection with a one-time code. |
| `remarkable_read` | Returns one page as one bounded JPEG, with optional crop and detail level. |
| `remarkable_browse` | Lists one folder at a time. |
| `remarkable_search` | Searches document names, paths, and tags. |
| `remarkable_recent` | Lists recently modified documents. |
| `remarkable_status` | Checks the cloud connection. |
| `remarkable_upload` | Uploads one local PDF or EPUB, up to 512 MiB. |
| `remarkable_mkdir` | Creates a folder. |
| `remarkable_move` | Moves a document or folder. |
| `remarkable_rename` | Renames a document or folder. |
| `remarkable_delete` | Moves a document or folder to cloud trash. |

There are deliberately no separate image, canvas, OCR, or bulk-export tools.

## Page images

`remarkable_read` always returns exactly one image and one short page label.

- Standard: up to 1600 px on the long edge and 1 MiB.
- High detail: up to 2048 px and 1.5 MiB.
- Crops use normalized page coordinates and are rendered at extra resolution before being resized.
- PDF pages are rasterized locally; reMarkable ink is composited over the page.
- Notebook ink is parsed and rendered locally.

The server does not run OCR. Handwriting remains visual, and search does not inspect handwritten words. EPUB page metadata and ink can be rendered, but the EPUB text underlay is not laid out locally.

## Privacy and credentials

- Cloud requests go directly from this server to reMarkable.
- The connection flow uses [reMarkable's official device page](https://my.remarkable.com/device/desktop/connect).
- Tokens are stored in your platform configuration directory and set to owner-only permissions on Unix.
- You can provide a device token with `REMARKABLE_TOKEN` instead of storing one on disk.
- Tool errors are short and never include a connection code or token.

## Installation options

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/Bradley-Butcher/remarkable-mcp/main/install.sh -o install-remarkable-mcp.sh
bash install-remarkable-mcp.sh v0.1.0
```

Choose another install directory:

```bash
REMARKABLE_MCP_INSTALL_DIR="$HOME/bin" bash install-remarkable-mcp.sh
```

Build from source with Rust 1.88 or newer:

```bash
git clone https://github.com/Bradley-Butcher/remarkable-mcp.git
cd remarkable-mcp
cargo build --release --locked
```

The binary will be at `target/release/remarkable-mcp`.

## Troubleshooting

**The server command is not found**

Make sure `~/.local/bin` is in `PATH`, then fully restart the MCP client. You can also use the binary's absolute path in the client configuration.

**The connection code is rejected**

One-time codes expire. Ask the assistant to connect again, generate a fresh code, and paste it without surrounding text.

**A document name is ambiguous**

Browse first, then use the full library path returned by the server.

**An EPUB page has no text underlay**

EPUB layout is produced by the tablet and is not reproduced by this local renderer. PDF and notebook pages have fuller visual support.

## Development and releases

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

Pushing a `vX.Y.Z` tag runs the release workflow. Tests must pass before Linux, macOS, and Windows archives and SHA-256 checksums are attached to the release.

## Project status

This project uses reMarkable's undocumented cloud sync protocol, which may change without notice. It is not affiliated with or endorsed by reMarkable AS.

Licensed under the [MIT License](LICENSE).
