# Image storage choices

For an archive with about 20 GB of images and 12 GB of messages, use compact
local images if offline viewing matters. Use geometric avatars when profile
pictures add little value. The viewing target here is **300 milliseconds
(0.3 seconds)** for an already saved image. Actual corpus savings and latency
must be measured on the user's machine; the private archive was unavailable
during development.

Cutting images from 20 GB to 10 GB saves 50% of image storage and about 31% of
the combined 32 GB archive. It does not halve the whole project.

## Research and alternatives

| Choice | Storage effect | Viewing and preservation |
| --- | --- | --- |
| Geometric profiles | Eliminates future avatar/banner downloads | Existing deterministic geometric message avatars display immediately; message attachment policy is independent |
| No new media downloads | No additional binary cache growth | Existing saved files can still display; uncached images depend on Discord and network access |
| Compact local WebP | Savings depend on input format, dimensions and content | Direct access to each file; no archive extraction or encoding for already saved views; loses image detail |
| Original files | Preserves received binary bytes | Most storage; appropriate for original-resolution screenshots, evidence, animation and other assets |
| AVIF | Candidate for a later measured comparison | Do not assume that smaller files meet the same decode target on every device; not implemented here |
| ZIP/gzip around the directory | Not a dependable route to 50% savings on already compressed images | Adds an extra storage layer; does not reduce pixel decoding work |

Google reports lossless WebP averaging 26% smaller than PNG, and lossy WebP
25–34% smaller than comparable JPEG at equivalent SSIM. These are study
results, not a promise about re-encoding existing files. Achieving 50% often
requires reducing dimensions, accepting additional loss, eliminating unnecessary
images, or removing duplicates.
[Source: Google's WebP documentation](https://developers.google.com/speed/webp).

Keeping attachment URLs alone is not a durable offline archive. Discord's
attachment CDN links have expiration times; obtaining a fresh URL may require
fetching the original message while it remains accessible. Standard avatar
endpoints are not signed in the same way. Remote fetch latency cannot be given
a universal 300 ms bound.
[Source: Discord CDN reference](https://docs.discord.com/developers/reference#signed-attachment-cdn-urls).

The existing cache already merges repeated references to the same canonical
Discord URL, ignoring attachment signatures. It does not merge identical bytes
uploaded under different URLs. The audit reports potential exact duplicate bytes
from recorded digests; cross-URL deduplication is not part of this change.

## Configure future collection

Install updated dependencies with `python -m pip install -r requirements.txt`.
The collector adds Pillow; the standalone read-only viewer can serve compact
images without Pillow or the optimizer installed.

`SEARCHCORD_MEDIA_MODE` accepts:

- `original` (default): retain downloaded bytes, preserving existing behavior.
- `compact`: after a new download, attempt a WebP conversion. Keep the original
  when conversion is unsupported, fails, or would increase size. When conversion
  succeeds, publish the verified compact representation and retire that input.
- `none`: skip all new cached binary downloads, including downloads requested
  by the collector's image-opening endpoint. This does not delete existing files
  or block the browser from following original remote links.

`SEARCHCORD_PROFILE_IMAGES` accepts `original` (default) or `shapes`. `shapes`
returns no avatar/banner presentation URLs and skips their downloads. Message
rows keep their existing geometric shapes; profile cards keep their existing
initials fallback. Saved profile metadata remains intact. Set this in both
collector and viewer processes if both should hide profile imagery.

For compact attachments with geometric avatars, in PowerShell before starting:

```powershell
$env:SEARCHCORD_MEDIA_MODE = "compact"
$env:SEARCHCORD_PROFILE_IMAGES = "shapes"
python app.py
```

For no new cached media, change `compact` to `none`. On Bash:

```sh
SEARCHCORD_MEDIA_MODE=compact SEARCHCORD_PROFILE_IMAGES=shapes python app.py
```

These settings affect future downloads and presentation. Existing saved objects
are not silently rewritten or deleted. Compact mode does not re-encode a
previously saved original when the collector encounters its URL again; use the
offline migration below.

## Audit and migrate an existing archive

Use the actual database path, which may differ when `SEARCHCORD_DATA_DIR` is set.
These commands never open or modify SQLite; they locate its sibling `media/`
directory. Only Searchcord's manifest-backed cache files are candidates, not
arbitrary downloaded images elsewhere in the project.

```sh
python media_optimize.py data/searchcord.db audit
python media_optimize.py data/searchcord.db optimize --limit 200
python media_optimize.py data/searchcord.db optimize
```

The audit separates avatar, banner and attachment bytes and recorded MIME types.
`disk_bytes` includes owned manifests and retained binaries; `active_bytes`
counts representations currently referenced by valid manifests. It reports
file lengths, not filesystem allocated-block usage. Missing or invalid objects
are counted. Audit duplicate estimates trust the saved hashes; conversion and
serving independently verify file contents.

`optimize` is a **dry run** by default: it decodes and encodes candidate images
to calculate actual candidate sizes without writing files. `--limit` is a quick
bounded trial in directory order, not a statistically representative estimate.
Use the full run for the archive-wide result. `active_saving_percent` measures
only verified processed objects; examine error and skip counts alongside it.
Animation, unsupported formats, non-image files and images that would grow are
kept and contribute no savings. Pixel-limited images are also kept.

Stop collection and close viewers before applying a migration. Back up the
database and entire `media/` directory, with enough disk space for compact output
and retained inputs. Preview a converted copy before discarding source detail:

```sh
python media_optimize.py data/searchcord.db optimize --apply
```

This publishes compact files but **keeps originals**, so disk use initially
increases. To reclaim space after review and backup:

```sh
python media_optimize.py data/searchcord.db optimize --apply --remove-originals
python media_optimize.py data/searchcord.db audit
python media_optimize.py data/searchcord.db benchmark --limit 200
```

The second apply run recognizes compact objects and retires only the original
`.bin` whose size and SHA-256 match the recorded input. `retired_bytes` counts
removed source bytes, not net savings: compare `disk_bytes_before` and
`disk_bytes_after`, or audits taken before and after the complete migration.
There is no automatic inverse for lossy conversion; rollback requires restoring
the backup. Older Searchcord versions do not understand compact manifests, so
restore original manifests and binaries before downgrading.

Selecting `none` or `shapes` does not reclaim existing media by itself. To stop
keeping the entire cache, stop both processes, back it up externally, and remove
only the database's sibling `media/` directory. SQLite message/profile metadata
and original remote URLs remain. Do not use **Clear All Data**, which also removes
archived messages and profiles.

## Encoding and file integrity

The first compact preset uses WebP quality 80, method 4, preserving aspect ratio
and limiting the longest edge to 128 pixels for avatars, 1024 for banners, and
1600 for attachments. It never enlarges an image. Alpha and EXIF orientation
are preserved; ICC profiles are carried through. EXIF/XMP metadata is omitted.
Animated/multiframe images, unsupported color modes, images over 32 million
pixels and input files over the collector's 25 MiB limit stay original. Fine
text in screenshots may suffer; retain originals when exact pixels matter.
[Pillow WebP options](https://pillow.readthedocs.io/en/stable/handbook/image-file-formats.html#webp),
[orientation handling](https://pillow.readthedocs.io/en/stable/reference/ImageOps.html#PIL.ImageOps.exif_transpose).

Objects remain independently readable. Compact data lives at
`<url-key>.<content-sha256>.webp`, selected by the existing manifest. Publication
writes and verifies the new file before atomically replacing that manifest;
only then can the original be removed. A process interruption before publication
leaves the original selected; afterward the compact object is selected. A failed
publication may leave an unreferenced compact file, reported in disk usage and
removed by the normal owned-cache clear. This is process-interruption ordering,
not a power-loss durability guarantee.

The `/api/media/<url-key>` endpoint still checks digest, rejects symlinks and
arbitrary manifest paths, and keeps its private `no-store` response policy.
Snapshot export copies only the selected verified representation. No database
URL rewrite or schema migration is required. Live encoding runs off the event
loop, using the existing four download workers; a cancelled writer joins its
encoding task before exiting. An in-flight conversion can extend the page's
download budget. No conversion occurs on the already-saved media serving route.
Opening an uncached remote attachment may download and convert it, and is
outside the local viewing target.

## Measurements and the 300 ms target

Development experiment: five photos from
[Google's public WebP gallery](https://developers.google.com/speed/webp/gallery1),
using both supplied JPEG and PNG versions (ten files). The inputs are already
within the 1600-pixel attachment limit, so this experiment mainly measures
re-encoding. These versions show format sensitivity; they are not ten independent
photos or a representative sample of a Discord archive.

| Inputs | Before | Compact | Saved |
| --- | ---: | ---: | ---: |
| Five JPEG versions | 818,122 B | 726,624 B | 11.18% |
| Five PNG versions | 6,722,723 B | 758,328 B | 88.72% |
| Combined | 7,540,845 B | 1,484,952 B | 80.31% |

Do not extrapolate the combined number to the private 20 GB collection. Google
used a different, target-size encoder setup in its published gallery; the
measurements above are from this implementation's fixed quality-80 preset.

On this Linux cloud environment, Python 3.12, Pillow 12.3.0 and headless system
Chromium, opening each compact file once over localhost with Chromium's HTTP
cache disabled measured median 46.9 ms, sample p95 82.14 ms, maximum 93.3 ms.
Timing covered setting the image URL, HTTP transfer, initial server digest check,
`image.decode()`, insertion, and two animation frames. The digest cache was
cleared before the run; OS file caches were uncontrolled. Local disk read plus
fresh digest plus Pillow first-frame decode measured median 10.383 ms and maximum
22.742 ms. Percentiles use linear interpolation between observations. The
experiment did not include network hosting, concurrent scrolling,
search time, low-end hardware, animations, or the user's actual archive.

The shipped `benchmark` command reports local read + fresh SHA-256 + first-frame
Pillow decode, p50/p95/max, skips and counts at or above 300 ms. It samples by
URL-key hash across the full manifest list. It does not measure browser display
or flush OS caches. In the actual browser, measure uncached and repeated opening,
scrolling concurrency, large images and representative devices separately.
Sub-300 ms is a validation target, not a universal bound for every image.
