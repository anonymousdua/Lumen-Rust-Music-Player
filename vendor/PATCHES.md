# Vendored crates

## `gpui` 0.2.2 (crates.io), patched

`vendor/gpui` is an unmodified copy of the crates.io release `gpui = "0.2.2"`
(Zed's UI framework, Apache-2.0, see `vendor/gpui/LICENSE-APACHE`) plus the three
changes below. The workspace uses it through `[patch.crates-io]` in the root
`Cargo.toml`. To drop the patch, delete that `[patch]` entry and this folder.

### 1. Bundled fonts as font fallbacks on Windows (`src/platform/windows/direct_write.rs`)

**Why:** upstream `generate_font_fallbacks` looks fallback families up only in the
*system* font collection. Fonts registered with `TextSystem::add_fonts` live in a
separate custom collection, so the bundled Noto Sans Devanagari / Malayalam
fallbacks were silently ignored ("No matching font found") and Windows fell back
to whatever system font covers the script (Nirmala UI). The spec says never to
rely on installed fonts.

**Change:** search the custom collection first, then the system collection, and
pass the collection that matched to `IDWriteFontFallbackBuilder::AddMapping`.

**Verify:** `target\debug\lumen.exe --font-check` prints which face shaped each
sample; Hindi and Malayalam must show `Noto Sans …` at the requested weight.

### 2. Warning fix (`src/taffy.rs`)

Two float literals get an explicit `_f32` suffix. Newer rustc warns about the
inferred fallback, and because a path dependency's warnings are shown in our
build, this keeps `cargo build` output clean. No behavior change.

### 3. Find `fxc.exe` in any installed Windows SDK (`build.rs`)

**Why:** release builds compile the DirectX shaders with `fxc.exe`. Upstream
checks `GPUI_FXC_PATH`, then `PATH`, then exactly SDK `10.0.26100.0`, so release
builds failed on a machine that only has SDK 10.0.22621.

**Change:** after those checks, use the newest `Windows Kits\10\bin\<ver>\x64\fxc.exe`
that exists. Setting `GPUI_FXC_PATH` still wins.

### Full diff

```diff
--- gpui-0.2.2/src/platform/windows/direct_write.rs	2006-07-24 05:21:28.000000000 +0400
+++ vendor/gpui/src/platform/windows/direct_write.rs	2026-11-26 19:12:49.607768600 +0400
@@ -330,23 +330,39 @@
         }
         unsafe {
             let builder = self.components.factory.CreateFontFallbackBuilder()?;
-            let font_set = &self.system_font_collection.GetFontSet()?;
+            // LUMEN PATCH (see vendor/PATCHES.md): look for fallback families in the
+            // fonts registered with `add_fonts` first, then in the system collection.
+            // Upstream only searched the system collection, so bundled fallback fonts
+            // were silently ignored.
+            let custom_font_set = self.custom_font_collection.GetFontSet()?;
+            let system_font_set = self.system_font_collection.GetFontSet()?;
             for family_name in fallbacks.fallback_list() {
-                let Some(fonts) = font_set
-                    .GetMatchingFonts(
-                        &HSTRING::from(family_name),
-                        DWRITE_FONT_WEIGHT_NORMAL,
-                        DWRITE_FONT_STRETCH_NORMAL,
-                        DWRITE_FONT_STYLE_NORMAL,
-                    )
-                    .log_err()
-                else {
-                    continue;
-                };
-                if fonts.GetFontCount() == 0 {
+                let mut found = None;
+                for (font_set, collection) in [
+                    (&custom_font_set, &self.custom_font_collection),
+                    (&system_font_set, &self.system_font_collection),
+                ] {
+                    let Some(fonts) = font_set
+                        .GetMatchingFonts(
+                            &HSTRING::from(family_name),
+                            DWRITE_FONT_WEIGHT_NORMAL,
+                            DWRITE_FONT_STRETCH_NORMAL,
+                            DWRITE_FONT_STYLE_NORMAL,
+                        )
+                        .log_err()
+                    else {
+                        continue;
+                    };
+                    if fonts.GetFontCount() > 0 {
+                        found = Some((fonts, collection.clone()));
+                        break;
+                    }
+                }
+                let Some((fonts, collection)) = found else {
                     log::error!("No matching font found for {}", family_name);
                     continue;
-                }
+                };
+                let collection: IDWriteFontCollection = collection.cast()?;
                 let font = fonts.GetFontFaceReference(0)?.CreateFontFace()?;
                 let mut count = 0;
                 font.GetUnicodeRanges(None, &mut count).ok();
@@ -364,7 +380,7 @@
                 builder.AddMapping(
                     &unicode_ranges,
                     &[target_family_name.as_ptr()],
-                    None,
+                    &collection,
                     None,
                     None,
                     1.0,
--- gpui-0.2.2/src/taffy.rs	2006-07-24 05:21:28.000000000 +0400
+++ vendor/gpui/src/taffy.rs	2026-11-26 19:18:20.245894700 +0400
@@ -309,7 +309,7 @@
             unit: &Option<u16>,
         ) -> Vec<taffy::GridTemplateComponent<T>> {
             // grid-template-columns: repeat(<number>, minmax(0, 1fr));
-            unit.map(|count| vec![repeat(count, vec![minmax(length(0.0), fr(1.0))])])
+            unit.map(|count| vec![repeat(count, vec![minmax(length(0.0_f32), fr(1.0_f32))])])
                 .unwrap_or_default()
         }
 
--- gpui-0.2.2/build.rs	2006-07-24 05:21:28.000000000 +0400
+++ vendor/gpui/build.rs	2026-11-26 19:26:44.113633000 +0400
@@ -353,6 +353,29 @@
                 .to_string();
         }
 
+        // LUMEN PATCH (see vendor/PATCHES.md): fall back to the newest installed
+        // Windows 10/11 SDK instead of requiring exactly 10.0.26100.0.
+        let kits_bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
+        if let Ok(entries) = fs::read_dir(kits_bin) {
+            let mut versions: Vec<(Vec<u32>, PathBuf)> = entries
+                .flatten()
+                .filter_map(|entry| {
+                    let name = entry.file_name().to_string_lossy().into_owned();
+                    let version: Option<Vec<u32>> =
+                        name.split('.').map(|part| part.parse().ok()).collect();
+                    let fxc = entry.path().join("x64").join("fxc.exe");
+                    match version {
+                        Some(v) if fxc.exists() => Some((v, fxc)),
+                        _ => None,
+                    }
+                })
+                .collect();
+            versions.sort();
+            if let Some((_, fxc)) = versions.pop() {
+                return fxc.to_string_lossy().into_owned();
+            }
+        }
+
         panic!("Failed to find fxc.exe");
     }
 
```
