# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""Upload a generated test image as TEMPORARY media.

Temporary media auto-expires in 3 days and is invisible to followers.
Useful for sanity-testing the media-upload endpoint without leaving traces.

Endpoint: POST cgi-bin/media/upload?type=image
Returns:  {"type": "image", "media_id": "...", "created_at": ...}

Run: uv run scripts/wechat-mp/04_upload_temp_media.py
"""

from __future__ import annotations

import httpx

from common import API_BASE, get_stable_token, make_test_image, pretty


def main() -> None:
    token = get_stable_token()
    image_path = make_test_image()
    print(f"Uploading {image_path.name} ({image_path.stat().st_size} bytes) as temp media...")

    with open(image_path, "rb") as f:
        resp = httpx.post(
            f"{API_BASE}/cgi-bin/media/upload",
            params={"access_token": token, "type": "image"},
            files={"media": (image_path.name, f, "image/png")},
            timeout=30.0,
        )
    resp.raise_for_status()
    data = resp.json()
    print(pretty(data))

    if "errcode" in data and data["errcode"] != 0:
        raise SystemExit(f"WeChat error: {data}")
    print(f"\n✓ temp media uploaded — expires in 3 days, no public exposure")


if __name__ == "__main__":
    main()
