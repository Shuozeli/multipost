# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""Upload a generated test image to the PERMANENT material library.

Permanent images live in the MP material library indefinitely (until
deleted via cgi-bin/material/del_material). They are NOT publicly
visible, but are required as `thumb_media_id` for drafts.

Endpoint: POST cgi-bin/material/add_material?type=image
Returns:  {"media_id": "...", "url": "https://mmbiz.qpic.cn/..."}

Stores media_id into artifacts.json so 06_create_draft.py can use it.

Run: uv run scripts/wechat-mp/05_upload_permanent_image.py
"""

from __future__ import annotations

import httpx

from common import (
    API_BASE,
    get_stable_token,
    load_artifacts,
    make_test_image,
    pretty,
    save_artifacts,
)


def main() -> None:
    token = get_stable_token()
    image_path = make_test_image()
    print(f"Uploading {image_path.name} as PERMANENT image material...")

    with open(image_path, "rb") as f:
        resp = httpx.post(
            f"{API_BASE}/cgi-bin/material/add_material",
            params={"access_token": token, "type": "image"},
            files={"media": (image_path.name, f, "image/png")},
            timeout=30.0,
        )
    resp.raise_for_status()
    data = resp.json()
    print(pretty(data))

    if "errcode" in data and data["errcode"] != 0:
        raise SystemExit(f"WeChat error: {data}")

    artifacts = load_artifacts()
    artifacts["permanent_image_media_id"] = data["media_id"]
    artifacts["permanent_image_url"] = data.get("url")
    save_artifacts(artifacts)
    print(f"\n✓ media_id saved to artifacts.json — ready for draft creation")


if __name__ == "__main__":
    main()
