mkdir -p "$out/bin"

gen_wrapper() {
  element="$1" elemshv="$2"
  echo "#!$bootstrap_tools/bin/bash"
  echo "exec $bootstrap_tools/bin/$element \$NIX_WRAPPER_${elemshv}_ARGS \"\$@\""
}

cd "$out/bin"

gen_wrapper gcc gcc > gcc
gen_wrapper g++ gxx > g++
$bootstrap_tools/bin/ln -sT gcc cc
