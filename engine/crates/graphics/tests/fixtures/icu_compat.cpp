#include "include/core/SkCanvas.h"
#include "include/core/SkPixmap.h"
#include "include/core/SkSurface.h"
#include "include/ports/SkFontMgr_empty.h"
#include "modules/skparagraph/include/ParagraphBuilder.h"
#include "modules/skparagraph/include/TypefaceFontProvider.h"
#include "modules/skunicode/include/SkUnicode_icu.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <unicode/udata.h>
#include <unicode/unum.h>
#include <unicode/uversion.h>
#include <vector>
static void require(bool ok, const char *message) {
  if (!ok) {
    std::fprintf(stderr, "%s\n", message);
    std::exit(2);
  }
}
int main(int argc, char **argv) {
  require(argc == 4, "usage: probe icudtl.dat font.ttf pixels.rgba");
  FILE *file = std::fopen(argv[1], "rb");
  require(file, "open data");
  std::fseek(file, 0, SEEK_END);
  long size = std::ftell(file);
  std::rewind(file);
  require(size > 0, "data size");
  void *data = std::aligned_alloc(16, (size + 15) & ~size_t(15));
  require(data, "aligned data");
  require(std::fread(data, 1, size, file) == size_t(size), "read data");
  std::fclose(file);
  UErrorCode status = U_ZERO_ERROR;
  udata_setFileAccess(UDATA_NO_FILES, &status);
  // Match V8's bootstrap: register common data, then load only requested
  // services. The game profile intentionally omits converters used by whole-ICU
  // u_init().
  udata_setCommonData(data, &status);
  if (U_FAILURE(status)) {
    std::fprintf(stderr, "ICU bootstrap: %s\n", u_errorName(status));
    return 2;
  }
  UVersionInfo version;
  u_getVersion(version);
  require(version[0] == U_ICU_VERSION_MAJOR_NUM,
          "ICU headers/code version mismatch");
  std::fprintf(stderr, "ICU %d.%d\n", version[0], version[1]);
  FILE *dump = std::fopen(argv[3], "wb");
  require(dump, "pixel dump");
  auto unicode = SkUnicodes::ICU::Make();
  require(bool(unicode), "SkUnicode factory");
  const char *texts[] = {"áb",
                         "我爱北京天安门",
                         "ภาษาไทยภาษาไทย",
                         "مرحبا abc بالعالم",
                         "नमस्ते दुनिया",
                         "👩‍👩‍👧‍👦🇨🇳👍🏽"};
  const char *locales[] = {"en", "zh", "th", "ar", "hi", "en"};
  for (int i = 0; i < 6; ++i) {
    for (int type = 0; type < 4; ++type) {
      auto iter =
          unicode->makeBreakIterator(locales[i], SkUnicode::BreakType(type));
      require(bool(iter), "break iterator");
      require(iter->setText(texts[i], int(std::strlen(texts[i]))),
              "break text");
      std::vector<int> offsets;
      for (int p = iter->first(); !iter->isDone(); p = iter->next())
        offsets.push_back(p);
      require(!offsets.empty() && offsets.front() == 0 &&
                  offsets.back() == int(std::strlen(texts[i])),
              "complete break range");
      if (i == 0 && type == 1)
        require(offsets == std::vector<int>({0, 3, 4}), "combining graphemes");
      if (i == 1 && type == 0)
        require(offsets == std::vector<int>({0, 3, 6, 12, 21}),
                "Chinese dictionary segmentation");
      if (i == 5 && type == 1)
        require(offsets == std::vector<int>({0, 25, 33, 41}),
                "emoji graphemes");
      std::printf("break %d %d", i, type);
      for (int p : offsets)
        std::printf(" %d", p);
      std::puts("");
    }
  }
  status = U_ZERO_ERROR;
  UNumberFormat *format =
      unum_open(UNUM_DECIMAL, nullptr, 0, "de_DE", nullptr, &status);
  require(U_SUCCESS(status) && format, "German format");
  UChar number[32];
  int32_t length =
      unum_formatDouble(format, 1234.5, number, 32, nullptr, &status);
  const UChar expected[] = {u'1', u'.', u'2', u'3', u'4', u',', u'5'};
  require(U_SUCCESS(status) && length == 7 &&
              !std::memcmp(number, expected, sizeof(expected)),
          "German decimal output");
  unum_close(format);
  auto mgr = SkFontMgr_New_Custom_Empty();
  require(bool(mgr), "font manager");
  auto typeface = mgr->makeFromFile(argv[2]);
  require(bool(typeface), "font file");
  auto provider = sk_make_sp<skia::textlayout::TypefaceFontProvider>();
  provider->registerTypeface(typeface, SkString("Probe"));
  auto fonts = sk_make_sp<skia::textlayout::FontCollection>();
  fonts->setAssetFontManager(provider);
  fonts->disableFontFallback();
  for (int i = 0; i < 6; ++i) {
    for (float width : {60.0f, 120.0f, 300.0f}) {
      skia::textlayout::ParagraphStyle style;
      skia::textlayout::TextStyle text;
      text.setFontFamilies({SkString("Probe")});
      text.setFontSize(20);
      text.setLocale(SkString(locales[i]));
      style.setTextStyle(text);
      auto builder =
          skia::textlayout::ParagraphBuilder::make(style, fonts, unicode);
      require(bool(builder), "paragraph factory");
      builder->addText(texts[i]);
      auto paragraph = builder->Build();
      paragraph->layout(width);
      require(std::isfinite(paragraph->getHeight()) &&
                  paragraph->getHeight() > 0,
              "paragraph height");
      auto surface = SkSurfaces::Raster(SkImageInfo::MakeN32Premul(320, 160));
      require(bool(surface), "raster surface");
      surface->getCanvas()->clear(SK_ColorWHITE);
      paragraph->paint(surface->getCanvas(), 0, 0);
      SkPixmap pixels;
      require(surface->peekPixels(&pixels), "raster pixels");
      uint64_t hash = 14695981039346656037ull;
      const auto *bytes = static_cast<const uint8_t *>(pixels.addr());
      for (size_t p = 0; p < pixels.computeByteSize(); ++p)
        hash = (hash ^ bytes[p]) * 1099511628211ull;
      require(std::fwrite(bytes, 1, pixels.computeByteSize(), dump) ==
                  pixels.computeByteSize(),
              "write pixels");
      std::vector<skia::textlayout::LineMetrics> lines;
      paragraph->getLineMetrics(lines);
      std::printf("paragraph %d %.0f %.9g %.9g %zu %d %016llx\n", i, width,
                  paragraph->getHeight(), paragraph->getMaxIntrinsicWidth(),
                  lines.size(), paragraph->unresolvedGlyphs(),
                  (unsigned long long)hash);
    }
  }
  require(std::fclose(dump) == 0, "close pixel dump");
  std::puts("PASS SkUnicode / number / paragraph");
  // The common-data allocation must outlive ICU and every paragraph/font cache.
  return 0;
}
