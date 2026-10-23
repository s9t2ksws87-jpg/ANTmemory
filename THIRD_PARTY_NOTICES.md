# Third-Party Notices

ANTmemory includes derived work and depends on third-party open-source software. Those components retain their original licenses.

## agent-memory-store

- Upstream: https://github.com/vbfs/agent-memory-store
- Original package version used by ANTmemory: 0.0.9
- Author declared by upstream package metadata: Vinícius Barreto
- Declared license: MIT

The native storage, migration, search, and embedding code retains derivations from this project. ANTmemory 2.0.0 owns and maintains those modules locally; it no longer installs `agent-memory-store` or patches its installed files. Removal of the dependency does not remove attribution or the license obligations of derived code.

MIT License — original author attribution: Vinícius Barreto (`vbfs`).

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Runtime dependencies and model

The MCP SDK, Hugging Face transformers runtime, Zod, gray-matter, their transitive dependencies, and platform-specific runtime packages retain their own licenses. Dependency versions and package license metadata are recorded in `server/package-lock.json` and installed package files. Preserve their required notices when redistributing dependencies.

The `Xenova/all-MiniLM-L6-v2` model is downloaded separately from Hugging Face; it is not bundled into this repository. Its model card and accompanying license govern the model files: https://huggingface.co/Xenova/all-MiniLM-L6-v2

ANTmemory's MIT license covers its independently owned code, documentation, and tests; it does not relicense third-party components.
