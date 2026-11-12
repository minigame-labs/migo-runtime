"""One shipping dependency list for Gradle, Maven staging and verification."""
from __future__ import annotations

import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]


def load(root: Path = ROOT) -> dict:
    data = json.loads((root / "contracts/android-runtime-dependencies.json").read_text())
    if data.get("schema") != 1 or set(data) != {"schema", "maven_runtime", "bundled_jars"}:
        raise ValueError("invalid Android runtime dependency schema")
    seen = set()
    for dep in data["maven_runtime"]:
        if set(dep) != {"group", "artifact", "version"} or any(
            not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+", value)
            for value in dep.values()
        ):
            raise ValueError("invalid fixed Android Maven dependency")
        coordinate = (dep["group"], dep["artifact"])
        if coordinate in seen:
            raise ValueError("duplicate Android Maven dependency")
        seen.add(coordinate)
    for jar in data["bundled_jars"]:
        path = Path(jar)
        if path.is_absolute() or ".." in path.parts or path.suffix != ".jar" or not (root / path).is_file():
            raise ValueError("invalid bundled Android jar")
    return data


def check_gradle(root: Path = ROOT) -> None:
    load(root)
    script = (root / "platforms/android/library/build.gradle").read_text()
    declarations = re.findall(
        r"^\s*([A-Za-z]*(?:[Ii]mplementation|[Aa]pi|[Rr]untimeOnly)\b[^\n]*)", script, re.M
    )
    shipping = [d.strip() for d in declarations if not d.startswith(("test", "androidTest"))]
    expected = [
        'implementation "${dependency.group}:${dependency.artifact}:${dependency.version}"',
        'implementation files(rootProject.file("../../${jar}"))',
    ]
    if shipping != expected or any(token not in script for token in (
        "contracts/android-runtime-dependencies.json", "migoRuntimeDependencies.maven_runtime.each",
        "migoRuntimeDependencies.bundled_jars.each",
    )):
        raise ValueError("Gradle shipping dependencies must come only from the canonical Android runtime list")


CONSUMER_LOCK = "tests/maven_consumer/android/gradle.lockfile"
# What a host's app runs with, each way it can take the engine.
CONSUMER_RUNTIME = ("bundledDebugRuntimeClasspath", "deferredDebugRuntimeClasspath")


def check_consumer_lock(root: Path = ROOT) -> None:
    """The host app that resolves the published SDK (tests/maven_consumer) locks its dependencies, and a
    runtime dependency the SDK's POM gains reaches that app: one its lock does not hold fails its build --
    which no gate saw until the whole release had been built, a day of red after ZXing was added."""
    locked: dict[str, set[str]] = {}
    for line in (root / CONSUMER_LOCK).read_text().splitlines():
        coordinate, separator, configurations = line.partition("=")
        if separator and not line.startswith("#"):
            locked[coordinate] = set(configurations.split(","))
    for dep in load(root)["maven_runtime"]:
        coordinate = f"{dep['group']}:{dep['artifact']}:{dep['version']}"
        missing = [name for name in CONSUMER_RUNTIME if name not in locked.get(coordinate, set())]
        if missing:
            raise ValueError(
                f"{CONSUMER_LOCK} does not lock {coordinate} for {', '.join(missing)}: regenerate it against a "
                "staged repository (gradlew :maven-consumer:dependencies --write-locks -PmigoMavenRepo=<repo>)"
            )


if __name__ == "__main__":
    check_gradle()
    check_consumer_lock()
    print("Android Gradle runtime dependencies agree with canonical Maven list, and the consumer app locks them")
