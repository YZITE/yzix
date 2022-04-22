#!@bootstrapTools@/bin/bash -e

NIX_WRAPPER_gcc_ARGS="-Wl,--dynamic-linker=@bootstrapTools@/lib/ld-linux.so.2"

for path in @bootstrapTools@/include-glibc \
  @bootstrapTools@/lib/gcc/i686-unknown-linux-gnu/8.3.0/include \
  @bootstrapTools@/lib/gcc/i686-unknown-linux-gnu/8.3.0/include-fixed; do
  NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -idirafter $path"
done

NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -Wl,--rpath=@bootstrapTools@/lib -I @bootstrapTools@/include"
PATH="$PATH:@wrappers@/bin:@bootstrapTools@/bin"

for dep in $buildInputs; do
  NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -Wl,--rpath=$dep/lib -I $dep/include"
  PATH="$PATH:$dep/bin"
done

NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gcc_ARGS"

for path in @bootstrapTools@/include/c++/8.3.0 \
  @bootstrapTools@/include/c++/8.3.0/i686-unknown-linux-gnu \
  @bootstrapTools@/include/c++/8.3.0/backward; do
  NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gxx_ARGS -idirafter $path"
done

export PATH
export NIX_WRAPPER_gxx_ARGS
export NIX_WRAPPER_gcc_ARGS

ln -sT @bootstrapTools@/bin /bin

if ! [ -d "$src" ]; then
  tar -xf "$src"
  sourceRoot="$(ls | head -1)"
  echo sourceRoot is "$sourceRoot"
  cd "$sourceRoot"
else
  cd "$src"
fi

type -p cpp
env

"$@" || exit 1

for path in $(find "$out/bin" "$out/lib" -type f -executable); do
  echo Shrinking RPATH of "$path"
  patchelf --shrink-rpath "$path" || true
done
