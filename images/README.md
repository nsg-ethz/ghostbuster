# Router images

`frr-images-min.tar.gz` (550 MB) holds the seven FRR router images the GNS3 experiments run. It is
stored in this repository with [Git LFS](https://git-lfs.com), so `git-lfs` has to be installed
before cloning:

```sh
git lfs install
git clone --recurse-submodules https://github.com/nsg-ethz/ghostbuster.git
```

If you cloned without LFS, `git lfs pull` fetches it afterwards. A missing LFS setup is easy to
spot: the file is then a ~130 byte text pointer rather than a 550 MB archive.

Load the images into your Docker daemon, from the repository root:

```sh
./gns3/load-images.sh
```

`frr-images-min.tar.gz.sha256` is the checksum of the archive.

## What is in the archive

| Image | Used by |
|---|---|
| `frr:latest` | Default image for every non-faulty router |
| `frr:10.2.1` | Unmodified release for `lp_bug` and `mrai_bug`, used when `GROUND_TRUTH` is off |
| `frr:8.4.2` | `pl_bug` |
| `frr:8.5.1` | `test_gns3_different_routers` |
| `frr:gns-alpine-lp-bug` | `lp_bug` ground truth — patched FRR |
| `frr:gns-alpine-mrai-bug` | `mrai_bug` ground truth — patched FRR |
| `gns3/ipterm:latest` | GNS3 terminal helper node |

## The two instrumented images

`frr:gns-alpine-lp-bug` and `frr:gns-alpine-mrai-bug` are FRR builds patched to log a
`<<<:::BUG:::>>>` marker at the moment the bug fires, which is what the ground-truth checker greps
for. The patch adds that marker — it does **not** introduce the bug. Both bugs are present in
unmodified released FRR (`frr:10.2.1`), which is what those experiments run when `GROUND_TRUTH` is
off; you simply lose the independent confirmation.

They cannot be rebuilt from anything in this repository — the archive is the only copy.

The changes themselves live in the branches of this fork of FRR, for anyone who wants to read the
actual diffs:

**https://github.com/tiborschneider/frr**
