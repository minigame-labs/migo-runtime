"""The Android permission surface each product profile ships, declared once.

Two gates read this: `test-permission-coverage-contract.sh`, which holds the *source*
manifests to it, and `test-android-merged-manifest-permissions.sh`, which holds the
*merged* manifests Gradle actually produces to it. A second copy of the table would be
two statements of one rule, and the one that never ships is the one the tests end up
over -- so the table lives here and neither gate restates it.

`maxSdkVersion` is part of the policy, not decoration: it is what decides which
permissions a consumer application is actually asking for on a given Android version,
so the value is pinned exactly rather than merely required to be present.
"""

from __future__ import annotations

import xml.etree.ElementTree as ET

ANDROID_NS = "{http://schemas.android.com/apk/res/android}"

# Declared by the Full profile only. A Slim build must not carry any of them: that is
# the product promise, and a merged manifest is where a dependency could break it.
FULL_PERMISSION_POLICY: dict[str, str | None] = {
    "android.permission.CAMERA": None,
    "android.permission.RECORD_AUDIO": None,
    "android.permission.BLUETOOTH": "30",
    "android.permission.BLUETOOTH_ADMIN": "30",
    "android.permission.BLUETOOTH_CONNECT": None,
    "android.permission.BLUETOOTH_SCAN": None,
    "android.permission.ACCESS_COARSE_LOCATION": None,
    "android.permission.ACCESS_FINE_LOCATION": None,
    "android.permission.WRITE_EXTERNAL_STORAGE": "28",
    # Install-time ("normal"): the window's screen-recording state, Android 15+.
    "android.permission.DETECT_SCREEN_RECORDING": None,
    # Install-time ("normal"): screenshots of the game's Activity, Android 14+.
    "android.permission.DETECT_SCREEN_CAPTURE": None,
}

# Carried by every profile. None of these is a runtime permission, which is why they
# are not part of the Full/Slim distinction.
BASE_PERMISSIONS: dict[str, str | None] = {
    "android.permission.INTERNET": None,
    "android.permission.ACCESS_NETWORK_STATE": None,
    "android.permission.VIBRATE": None,
}


def manifest_permissions(source: str) -> tuple[dict[str, str | None], list[str]]:
    """Maps each declared permission to its `maxSdkVersion`, or None if unbounded.

    Both declaration forms are read. `<uses-permission-sdk-23>` requests a permission
    only from API 23 upwards, which on this library -- whose floor is API 26 -- is every
    supported device, so ignoring it would let a sensitive permission through both the
    source and the merged-manifest gate while looking like a different element.
    """
    found: dict[str, str | None] = {}
    problems: list[str] = []
    try:
        manifest = ET.fromstring(source)
    except ET.ParseError as error:
        return {}, [f"invalid Android manifest XML: {error}"]
    for tag in ("uses-permission", "uses-permission-sdk-23"):
        for element in manifest.findall(tag):
            name = element.get(ANDROID_NS + "name")
            if not name:
                problems.append(f"{tag} without android:name")
                continue
            if name in found:
                problems.append(f"duplicate manifest permission `{name}`")
                continue
            found[name] = element.get(ANDROID_NS + "maxSdkVersion")
    return found, problems


def effective_permissions(permissions: dict[str, str | None], api: int) -> set[str]:
    """The permissions a consumer actually requests when running on `api`.

    `maxSdkVersion` is inclusive: a permission bounded at 28 is still requested on
    API 28 and dropped from API 29 onwards.
    """
    effective = set()
    for name, max_sdk in permissions.items():
        if max_sdk is None or api <= int(max_sdk):
            effective.add(name)
    return effective
