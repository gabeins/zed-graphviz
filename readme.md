# Graphviz for Zed

An extension that adds Graphviz support for Zed.

<img src="./img/syntax-highlight.png" alt="A snippet of Graphviz code in the Zed editor, with the syntax highlighted" />

Learn more about Graphviz at <https://graphviz.org>

## Installation

1. Open the Zed Extension Gallery by:<br />
  a. pressing `cmd-shift-x` (macOS)<br />
  b. or pressing `ctrl-shift-x` (Linux)<br />
  c. or triggering the `zed: extensions` command from the command pallet<br />
  d. or by selecting `"Zed > Extensions"` from the menu bar
2. Search for `"Graphviz"`.
3. Click `"Install"`.

## Preview

The extension can show a rendered preview of the graph in a Zed tab. It needs
Zed 1.21 or newer.

### Opening the preview

1. Open a `.dot` or `.gv` file.
2. Open the code actions menu with `cmd-.` (macOS) or `ctrl-.` (Linux and
   Windows).
3. Choose **Open Graphviz preview**.

The graph opens as `<name>.png` in Zed's image viewer, where you can zoom in
and out, fit the image or show it at actual size. Running the action again
brings the same tab back to the front.

The preview is a normal Zed tab. Go back to the source with `pane: go back`
or by switching tabs, or drag the tab to the side (or use **Split Right**
on it) to keep the source and the preview next to each other.

### Live updates and errors

While the preview is open it re-renders as you type, after a short pause.
Errors and warnings reported by Graphviz show up as diagnostics in the
editor; while the graph has an error the preview keeps showing the last
version that rendered.

Set `"render": "save"` to re-render only when the file is saved.

### Hover and export

Hover the `graph`/`digraph` line to see a thumbnail of the graph.

The code actions menu also has **Export graph as SVG** and **Export graph as
PNG**, which write `<name>.svg` or `<name>.png` next to the `.dot` file.
Exports always use Graphviz's normal light colours. An exported SVG keeps
the image references (`image=`) exactly as Graphviz wrote them.

### Renderer

The preview uses the Graphviz `dot` installed on your system when there is
one (Homebrew, apt, the Graphviz installer, ...). Without it, a copy of
Graphviz compiled to WebAssembly and built into the preview server is used,
so the preview works without installing anything. Set `"renderer"` to
`"dot"` or `"bundled"` to choose explicitly.

### Light and dark

Zed does not tell extensions which theme is active, so with the default
`"appearance": "auto"` the preview follows your operating system's light or
dark setting (which is also what Zed's `"mode": "system"` theme setting
follows). In dark mode the graph gets a dark background and light lines and
text, and colours set in the file win over these defaults. One difference
from Graphviz's usual behaviour: filled nodes and clusters that set only
`color` get a dark fill rather than that colour, so set `fillcolor` to
choose the fill. Set `"appearance"` to `"light"` or `"dark"` to pick one
regardless of the OS.

### Settings

Settings go in your Zed `settings.json`. All of them are optional:

```json
{
  "lsp": {
    "graphviz-preview": {
      "settings": {
        "renderer": "auto",
        "render": "live",
        "debounce_ms": 300,
        "timeout_ms": 10000,
        "scale": 2.0,
        "appearance": "auto"
      }
    }
  }
}
```

| Setting       | Default  | Description                                                                                            |
| ------------- | -------- | ------------------------------------------------------------------------------------------------------ |
| `renderer`    | `"auto"` | `"auto"` uses the system `dot` when found, else the built-in Graphviz. `"dot"` or `"bundled"` forces one. |
| `dot_path`    | none     | Path to the `dot` executable, if it is not on `PATH`.                                                  |
| `engine`      | none     | Layout engine (`dot`, `neato`, `fdp`, `sfdp`, `circo`, `twopi`, `osage`, `patchwork`). By default the file's `layout` attribute, or `dot`. |
| `render`      | `"live"` | `"live"` re-renders while typing, `"save"` only on save.                                               |
| `debounce_ms` | `300`    | Pause after the last keystroke before a live render.                                                   |
| `timeout_ms`  | `10000`  | A render that takes longer is stopped, so a huge graph cannot keep a CPU busy.                         |
| `scale`       | `2.0`    | Pixel density of the preview image.                                                                    |
| `appearance`  | `"auto"` | `"auto"` follows the OS light/dark setting; `"light"` or `"dark"` fixes it.                            |

Besides the timeout, the built-in Graphviz may use at most 1 GiB of
memory per render. A system `dot` is limited to 2 GiB of address space on
Linux; on macOS and Windows its memory is not limited, only its time.

To use your own build of the preview server, put `graphviz-preview` on your
`PATH`, or set `"lsp": {"graphviz-preview": {"binary": {"path": "..."}}}`;
Zed itself honours that setting (in trusted projects only), like it does for
every language server. Otherwise Zed downloads the server for your platform
from this repository's GitHub releases the first time a DOT file is opened;
the extension checks the download against the SHA-256 published with it
before using it.

Zed only starts language servers in projects you have trusted, so settings
from a project's own `.zed/settings.json`, such as `dot_path`, take effect
only in trusted projects.

### Privacy

Graphs are rendered locally, on your machine. The preview server makes no
network connections; the only download is the server itself, fetched once
from GitHub as described above.

Images referenced with `image=` are drawn in the preview only when they are
inside the workspace folder that holds the `.dot` file (or the file's own
folder); others are left out with a warning. A system `dot` still opens
such files to measure them, as it would on the command line.

### Building the preview server

The server lives in [`server/`](./server). To build it with the embedded
Graphviz:

```sh
server/graphviz-wasm/build.sh
cd server && cargo build --release
```

To build a server that only uses the system `dot`, run
`cargo build --release --no-default-features` in `server/` instead.

The embedded Graphviz is distributed under its own licence; see
[`server/graphviz-wasm/THIRD_PARTY_NOTICES.md`](./server/graphviz-wasm/THIRD_PARTY_NOTICES.md).

### Releasing

Pushing a `v<version>` tag that matches `extension.toml` runs the release
workflow: it runs the CI checks, builds the server for every platform,
and publishes the archives with their `.sha256` files on a new release.
It never replaces an existing release or asset. Enable GitHub's immutable
releases for the repository so published assets cannot be changed later.

## Acknowledgments

- Tree-sitter grammar by rydesun: [tree-sitter-dot](https://github.com/rydesun/tree-sitter-dot)
- Graphviz, the graph visualization software: <https://graphviz.org>
