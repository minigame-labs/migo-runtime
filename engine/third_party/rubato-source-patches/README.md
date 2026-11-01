# Published DSP metadata boundary

The seven exact published archive identities and original file SHA values are
recorded in ../rubato-sources.json. One metadata-only patch negates the three
upstream Cargo.lock ignore rules, retaining the published lockfiles alongside
the other provenance inputs. Rust, DSP parameters, licenses and Cargo manifests
remain identical to their published archives. Replay with patch -p0 --fuzz=0;
the product gate compares every resulting file with Root.
