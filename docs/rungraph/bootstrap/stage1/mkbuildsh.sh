set -e

$bootstrap_tools/bin/sed \
  -e "s#@bootstrapTools@#$bootstrap_tools#g" \
  -e "s#@wrappers@#$wrappers#g" \
  "$1" > "$out"

chmod +x "$out"
