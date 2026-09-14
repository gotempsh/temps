# Release runtime image consistency

Stable and nightly releases carry the same contract: every runtime image needed
by the binary is built from the release commit and selected by its immutable
multiarchitecture digest, not a moving `beta`, `latest`, or version alias.

## Publication order

1. Validate the release ref.
2. Build the three managed daemon/harness images, six legacy sandbox images,
   and preview gateway. Daemon lifecycle checks run on both AMD64 and ARM64
   before publication. At this stage only revision-specific tags are pushed.
3. Assemble `runtime-images.json` from each build's digest. Reject missing or
   duplicate entries, unexpected repositories, or mixed commit revisions.
   Inspect every published digest and require both Linux architectures.
4. Download that exact manifest in all four platform binary jobs. Compile with
   `TEMPS_RELEASE_IMAGE_MANIFEST` pointing at the file; invalid/incomplete
   manifests fail the build instead of silently falling back to stable tags.
5. After every binary succeeds, promote the verified digests to the appropriate
   stable or beta convenience aliases. These aliases are not used by the new
   release binaries.
6. Publish the GitHub release, including the manifest in every tarball and as a
   checksummed standalone asset.

An image or manifest verification failure prevents binary compilation and release
publication. A binary failure prevents channel promotion and release publication.
Already-published revision images may remain after a failed release; they are
not live channel promotion and no released binary points at them.

## Runtime selection

The manifest supplies defaults for managed application workspaces and isolated
credential checks, legacy preset sandboxes, and the preview gateway. Its values
are embedded at compile time; the manifest file is not required on the server.
Changing a runtime environment variable cannot switch these embedded defaults.
Explicit operator image overrides/pins remain operator choices. The exact
historical preview gateway default is recognized as a default and upgraded;
other custom gateway images are preserved.

Local source builds without a manifest retain development defaults. They do not
claim release consistency: use published release artifacts or supply a complete
verified manifest when building a deployable binary. This variable is a build
input, not a per-record runtime setting.

Dry-run releases build all dependencies and validate manifest completeness but
do not publish or check registry availability. Their artifacts are not deployable
release evidence; a non-dry-run release must pass the registry checks.

## Regression checks

```sh
bash .github/scripts/test-nightly-release-workflows.sh
python3 .github/scripts/test_release_image_manifest.py
python3 tools/sandbox-runtime/test_build_inputs.py
cargo test --lib -p temps-core release_manifest
```

The workflow contract rejects a binary job without a manifest dependency/input,
early channel promotion, and release publication before required images finish.
The old `0.3.4` versus `0.3.4-beta` mismatch cannot affect a manifest-built binary:
both stable and nightly use the exact recorded `repository@sha256:...` references.
