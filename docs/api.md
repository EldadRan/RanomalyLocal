# Ranomaly Local — API for calling apps

**Version 1** · for developers of apps that send jobs to Ranomaly Local

Ranomaly Local is a desktop app (macOS, Windows) that runs work a browser can't do on the user's own
machine: large downloads, video decoding, and writing thousands of files into a folder the user
picks.

A web app starts a job in three steps:

1. Publish a small JSON **manifest** at an https URL.
2. Open a **`ranomalylocal://` link** that points to the manifest.
3. Ranomaly Local takes over from there: it fetches the manifest and shows the user what will
   happen. It asks for folders and options, runs the job with progress and Cancel, then quits.

**Nothing is reported back to your app.** All feedback goes to the user, in Ranomaly Local's own
window, and that window never names the app that sent the job. It shows only the host the manifest
came from.

---

## 1 · Starting a job

### The link

```
ranomalylocal://run?manifest=<percent-encoded manifest URL>
```

```js
// Must run inside a click handler: browsers only open app links on a user gesture.
const link = "ranomalylocal://run?manifest=" + encodeURIComponent(manifestUrl);
window.location.href = link;
```

The link must be exactly this shape, or Ranomaly Local refuses it and fetches nothing:

- **Shape:** scheme `ranomalylocal`, host `run`, and no path.
- **Parameters:** exactly one, `manifest`, holding the whole URL encoded with
  `encodeURIComponent`. No other parameters and no `#fragment`.
- **Length:** at most **8 KB** in total.
- **The manifest URL** must be `https://` and carry no user name or password.

### When Ranomaly Local isn't installed

Browsers give no signal that a link did nothing, so detection is a heuristic:

- If the page fires `blur` or `visibilitychange` within about **1.5 s** of setting the link, the
  app opened.
- If not, show a panel with **Try again** and **Get Ranomaly Local** (download links below).
- Treat this as a hint, not a verdict. Keep the download link reachable anyway.

### The browser's permission prompt

The first time, the browser asks whether to open Ranomaly Local. How often it asks after that is up
to the user:

| Browser | Behaviour |
|---|---|
| Chrome, Edge | *"Always allow [site] to open links of this type"*. Ticked once, it never asks again for your site |
| Firefox | A similar per-site "always" option |
| Safari | Asks every time; there's nothing to remember |

**Recommended:** the first time a user starts a job, show a one-time note as the link fires. Skip
it on Safari.

> *Your browser will ask to open Ranomaly Local. Tick "Always allow" so it won't ask again.*

**Managed machines can skip the prompt.** IT sets the Chrome and Edge policy
`AutoLaunchProtocolsFromOrigins` to:

```json
[{ "protocol": "ranomalylocal", "allowed_origins": ["https://your-app.example"] }]
```

### One job at a time

A new link that arrives while a job is running is ignored, and the user is told to finish or
cancel the current job first. A new link arriving at any other time (waiting, setup, finished,
error) replaces what's on screen. After the user closes a finished or failed job, Ranomaly Local
quits, and the next link starts it again.

---

## 2 · The manifest

A JSON object of at most **64 KB**, served with `200` from the https URL in the link.

```json
{
  "version": 1,
  "op": "video_to_png",
  "job_id": "j_01J9XK2M",
  "title": "Harbour plate — №3",
  "…": "the op's own fields (section 4)"
}
```

The envelope fields are the same for every op:

| Field | Required | Meaning |
|---|---|---|
| `version` | yes | `1`. It changes only for breaking changes |
| `op` | yes | Which tool runs: `video_to_png` today. A version of Ranomaly Local that doesn't know the op tells the user it needs updating |
| `job_id` | no | Your reference, shown small on the setup screen. Useful for support. Up to 100 characters |
| `title` | no | What the user sees as the job's heading. Up to 200 characters. Each op has a fallback |

**Forward-compatible:** unknown fields are ignored, so you can add fields before Ranomaly Local
uses them. Text fields have control characters stripped.

**Serving the manifest:**

- **Answer `200` with the JSON.** `Content-Type: application/json` is recommended; the content
  type isn't checked.
- **Say "expired or used" with `403`, `404` or `410`.** The user is told the link was refused, with
  the code (see section 5).
- **Redirects** are followed, up to 5 hops, and only to https.
- **Single-use or short-lived URLs are fine.** Ranomaly Local fetches the manifest exactly once,
  when the link arrives.

---

## 3 · URLs inside a manifest

Every URL a manifest gives Ranomaly Local, such as a file to download, must be `https://` with no
user name or password. There is no host allowlist, and any https host works.

For **downloads**:

- **Plain `GET` returning the exact bytes.** No authentication headers are sent, so put any
  credential in the URL itself (a signed URL).
- **Support `Range` if you can** (`206` with `Content-Range: bytes <start>-…`). Ranomaly Local
  resumes after a dropped connection. A server that ignores `Range` still works, but a drop then
  restarts the download from the beginning.
- **Retries:**
  - `408`, `429` and `5xx` are retried with backoff.
  - `401`, `403`, `404` and `410` stop the job with a "start the job again" message.
  - Any other status stops the job with the code shown.
- **Timeouts:** 20 s to connect and 60 s without data. A stall counts as a drop and is resumed.
- **User-Agent:** requests identify as `Ranomaly-Local/<version>`.
- **Signed URLs:** give them hours, not minutes. The user chooses folders before the download
  starts, and a large file can take a long time. Ranomaly Local never refreshes a URL; if one
  expires the user starts the job again from your app, so **mint fresh URLs on every start and
  never reuse a link.**

---

## 4 · Ops

### `video_to_png`: a video as a numbered PNG sequence

This op downloads a video and decodes every frame to `frame_000001.png`, `frame_000002.png` and
so on, in a folder the user picks.

```json
{
  "version": 1,
  "op": "video_to_png",
  "job_id": "j_01J9XK2M",
  "title": "Harbour plate — №3",
  "input": {
    "url": "https://files.your-app.example/signed/…",
    "filename": "Harbour plate.mov",
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

| Field | Required | Meaning |
|---|---|---|
| `input.url` | yes | The video: https, following section 3 |
| `input.filename` | yes | A plain file name. It names the saved video and the frames folder. No `/ \ : * ? " < > \|`, no control characters, no leading dot, no trailing dot or space, not a Windows reserved name (`CON`, `NUL`, `COM1`…), up to 200 characters. **Sanitise before sending**, because anything else is refused |
| `input.size` | yes | Exact size in bytes, above 0. A download of any other size fails |
| `input.sha256` | no | 64 hex characters. When present, the download is verified against it |
| `input.expires_at` | no | Unix seconds when `input.url` stops working. Shown to the user as a countdown. **Send it** if the URL expires |
| `params.frames` | yes | Expected frame count, above 0. `round(duration × fps)` is fine. Used for the disk-space estimate and as the progress total. The real count comes from the file, and a mismatch is shown as a warning |
| `params.width`, `params.height` | yes | Pixel size, 1–65536. Used for the disk-space estimate before anything downloads |
| `params.fps` | no | Shown to the user |
| `params.pix_fmt` | no | An ffmpeg pixel format name, shown to the user. It is also a hint for the estimate; the file is probed for the real value |

The title falls back to `input.filename`.

**What the user does and sees:**

1. **Setup screen:** the file, its size, frames, resolution, format, link expiry and source host.
2. **The user's choices:**
   - **Frames folder.** The frames go into a new subfolder named after the file. If one exists and
     isn't empty, `(2)`, `(3)` and so on is added.
   - **Keep the downloaded video.** Off by default. When on, the user picks a folder for it;
     otherwise the video is a temporary file, deleted after decoding.
   - **PNG bit depth:** **8-bit** (the default) or **16-bit**. Transparency in the source is kept
     either way.
3. **Disk space** is checked before starting. If there clearly isn't enough, Start doesn't appear;
   if it may be tight, the screen warns.
4. **Run:** download, check, decode and finish, with progress and Cancel. Colour conversion honours
   the source's colour matrix and range. After a cancel or failure, the user keeps or deletes the
   frames already written.
5. **Done:** the frame count, the folder, and **Open folder**.

---

## 5 · What the user sees when something is wrong

Every failure before the job starts shows *"Could not run this job"* with one of these messages, and
nothing is downloaded. The messages name the problem in the manifest's own terms, so they can be
traced from a screenshot.

| Cause | Message |
|---|---|
| Link not the right shape, or over 8 KB | *the link is not a valid Ranomaly link* / *the link is too long* |
| Manifest or file URL not https | *the link must point to an https address* |
| Manifest URL answered `403` | *the request link was refused — it may have expired (HTTP 403). Start the job again* |
| Manifest URL answered `404` | *the request link was not found — it may have expired, been used already, or be wrong (HTTP 404). Start the job again* |
| Manifest URL answered `410` | *the request link has expired (HTTP 410). Start the job again* |
| Other server error, or no connection | *could not fetch the request: …* |
| Not JSON, or over 64 KB | *the request is not valid: it is not JSON* / *manifest is too large* |
| Field missing | *the request is not valid: missing params.width* |
| Wrong type | *the request is not valid: input.size must be a whole number* |
| Value out of range | *the request is not valid: input.filename is not a usable file name* (and similar) |
| Unknown `version` or `op` | *this request needs a newer version of Ranomaly Local* |

Once the job is running:

| Cause | Message |
|---|---|
| File URL refused mid-download | *the download link was refused — it may have expired (HTTP 403). Start the job again* |
| Wrong size or hash | *the downloaded file has the wrong size…* / *the downloaded file is corrupt (sha256 mismatch)* |
| Not a readable video | *ffprobe could not read the video: …* |

**"Start the job again" means your app's button again**, which must mint fresh URLs.

---

## 6 · Testing your integration

**Debug builds** of Ranomaly Local also accept `http://127.0.0.1:<any port>` for the manifest and
file URLs, so you can serve both from your machine without https. Release builds accept https only.

**`tools/mock_r2.py`** in this repository serves a manifest and a video locally and prints the link:

```sh
python3 tools/mock_r2.py --video clip.mov --open            # serves and opens the link (macOS)
python3 tools/mock_r2.py --video clip.mov --drop-at 0.3     # drop the connection at 30%: resume
python3 tools/mock_r2.py --video clip.mov --video-expires 30 --throttle 5   # expiry mid-download
```

**A checklist for your side:**

- The link opens Ranomaly Local with the right title, file, size, resolution and expiry.
- Re-fetching a used or expired manifest URL returns `403`, `404` or `410`.
- The not-installed panel appears when Ranomaly Local is absent.
- Your file names survive the rules in section 4. Test with spaces, unicode and dots.

## Download

These links always serve the newest release, so a *Get Ranomaly Local* button can use them as they
are:

| Platform | Link |
|---|---|
| Windows 10/11 (x64) | https://github.com/EldadRan/RanomalyLocal/releases/latest/download/RanomalyLocal-windows-x64-setup.exe |
| macOS 11+ (Apple silicon) | https://github.com/EldadRan/RanomalyLocal/releases/latest/download/RanomalyLocal-macos-arm64.dmg |
| All releases | https://github.com/EldadRan/RanomalyLocal/releases |

The Windows installer registers `ranomalylocal://`, installs for the current user and needs no
admin rights. On macOS, drag the app into Applications.

**Installers are not code-signed yet.**

- **Windows:** SmartScreen shows *"Windows protected your PC"*. Choose *More info → Run anyway*.
- **macOS:** the first launch needs right-click → *Open*.

Signing comes in a later release.
