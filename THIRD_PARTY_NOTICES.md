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

## Version 4 runtime and models

The Rust implementation uses the locked Cargo graph. Full per-dependency notices and metadata are in [third_party/rust/README.md](third_party/rust/README.md). ANTmemory-owned V4 code is MIT licensed; dependency licenses are not changed.

Windows distributions include Microsoft's ONNX Runtime 1.31.0 CPU libraries, under its MIT license and included upstream third-party notices: [third_party/onnxruntime](third_party/onnxruntime/). The runtime archive and extracted binaries are pinned in assets.lock.json.

The Ruri model is cl-nagoya/ruri-v3-70m by Hayato Tsukagoshi and Ryohei Sasano, under Apache-2.0. The bundled INT8 ONNX export is byte-matched to mochiya98/ruri-v3-70m-onnx. The file is renamed from model_int8.onnx to model.onnx without changing its bytes. The original model card, export card and license are in [third_party/ruri](third_party/ruri/); fixed revisions, sources and checksums are in assets.lock.json.

Historical JavaScript dependencies remain declared in legacy/v2/server/package-lock.json. Their packages and model weights are not part of the V4 binary distribution. No npm dependency is required by the native runtime or optional Node bridge.
