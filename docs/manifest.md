# Ranomaly Local — link and manifest contract (v1)

Ranomaly Local is a desktop helper that AAB (the browser app) starts to do work that a browser
cannot do locally. AAB writes a small JSON **manifest** to R2, then opens a
`ranomalylocal://` link that points at it. The helper downloads the manifest, shows the
user what it is about to do, asks for anything it needs (output folders, options), runs
the job and reports progress to the user. Nothing is reported back to AAB.

## 1. The link

```
ranomalylocal://run?manifest=<percent-encoded presigned R2 URL of the manifest>
```

The helper rejects the link unless **all** of these hold:

- scheme is `ranomalylocal`, host is `run`, path is empty or `/`
- the query has exactly one parameter, `manifest`, and nothing else (no fragment)
- the whole link is at most 8 KB
- the decoded manifest URL is https (section 3)

AAB must percent-encode the full presigned URL (`encodeURIComponent`). Pasting it
unencoded after the scheme breaks URL parsing.

## 2. The manifest

**Envelope and op fields.** The envelope is `version`, `op`, `job_id` and `title`, and it is the
same for every op. Every other field belongs to the op named in `op`. Section 4 onward has one
section per op. A helper that doesn't know the `op` says it needs updating.

`Content-Type: application/json`, at most 64 KB. Unknown fields are ignored so AAB can
add fields without breaking older helpers; `version` is bumped only for breaking changes.

```json
{
  "version": 1,
  "op": "video_to_png",
  "job_id": "j_01J9X…",
  "title": "Shot 012 — plate v3",
  "input": {
    "url": "https://<account>.r2.cloudflarestorage.com/<bucket>/<key>?X-Amz-…",
    "filename": "shot012_plate_v3.mov",
    "size": 51234567890,
    "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    "expires_at": 1790525700
  },
  "params": {
    "frames": 2880,
    "width": 7680,
    "height": 4320,
    "fps": 24,
    "pix_fmt": "yuv422p10le"
  }
}
```

| Field | Required | Notes |
|---|---|---|
| `version` | yes | Envelope. Must be `1`. |
| `op` | yes | Envelope. Which tool runs. Unknown ops are rejected with "update the helper". |
| `job_id` | no | Envelope. Shown to the user; helps support match a run to AAB. |
| `title` | no | Envelope. Human label shown in the window. Each op has a fallback (`video_to_png`: `input.filename`). |
| `input.url` | yes | Full URL of the source (https). |
| `input.filename` | yes | Plain file name, no directories. Used for the downloaded file and the frames folder name. |
| `input.size` | yes | Exact byte size; checked after download and used for the disk-space check. |
| `input.sha256` | no | Lower-case hex. When present the download is verified against it. |
| `input.expires_at` | no | Unix seconds when `input.url` stops working. Shown to the user. Send it: the URL format is CF's and is not parsed reliably. |
| `params.frames` | yes | Expected frame count; used for the disk estimate before download and for progress if the container does not report it. |
| `params.width`, `params.height` | yes | Used for the disk estimate before download. |
| `params.fps` | no | Informational. |
| `params.pix_fmt` | no | Informational; the helper probes the real value after download. |

### Link lifetime

The manifest link and the video link are ordinary presigned GETs. The helper does not
refresh them. If a link expires before or during the download the job fails with
"link expired — start again from AAB", and AAB issues a fresh manifest. Make the video
link live long enough for a large download plus the time the user spends choosing folders
(hours, not minutes).

## 3. Where the helper may fetch from

**Anywhere over https.** There is no host allowlist, so a well-formed link is the only trigger.
The rules:

- The manifest URL and every URL inside it are `https://`, with no user-info.
- Redirects are followed up to five hops, and never to plain http.
- The setup screen shows the manifest's host (*From*), so the user sees where a job came from
  before pressing Start. Nothing runs until they do.
- Debug builds also accept `http://127.0.0.1:<port>` for local mock servers.

## 4. Op `video_to_png`

Fields: `input.*` and `params.*` above.


1. Show title, file name, size, resolution, frame count and link expiry.
2. The user chooses:
   - **Frames folder**: frames go to `<folder>/<file stem>/frame_000001.png…`. If that
     folder already exists and is not empty, a numbered suffix is added.
   - **Keep the video**: off by default. When on, the user picks where the video is saved;
     otherwise it is downloaded next to the frames as a hidden temp file and deleted
     after decoding.
   - **Bit depth**: match source (default: 16-bit PNG for >8-bit sources, 8-bit otherwise),
     or force 8-bit or 16-bit. Alpha is kept when the source has it.
3. Disk space is checked against video size + frames × estimated PNG size. Not enough
   for the low estimate blocks the start; not enough for the high estimate warns.
4. Download with automatic resume on network drops (HTTP Range), verify size and sha256.
5. ffprobe, then ffmpeg with progress and Cancel. On Cancel or failure the user chooses
   to keep or delete the frames already written.
6. Done: frame count check and "Open folder".
