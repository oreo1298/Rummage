#!/bin/sh
# Refreshes the translation template (fossick.ts) and every translation (fossick_<lang>.ts)
# from the QML sources. Needs Qt 6's lupdate. To start a translation, copy fossick.ts to
# fossick_<lang>.ts (for example fossick_de.ts) and set its language attribute.
# fossick_en.ts holds only the English plural forms ("%Ln object(s)" → "1 object").
set -eu
cd "$(dirname "$0")/.."
LUPDATE=${LUPDATE:-$(command -v lupdate-qt6 || command -v lupdate || echo /usr/lib/qt6/bin/lupdate)}
for ts in translations/fossick.ts translations/fossick_*.ts; do
  [ -e "$ts" ] || continue
  plural=
  [ "$ts" = translations/fossick_en.ts ] && plural=-pluralonly
  "$LUPDATE" qml -extensions qml -source-language en -locations relative -no-obsolete $plural -ts "$ts"
done
