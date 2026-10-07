{ mkShell
, zerostack-toolchain
, lib

, stdenv
, zerostack
}:

mkShell {
  inputsFrom = [ zerostack ];
  LD_LIBRARY_PATH = lib.makeLibraryPath [ stdenv.cc.cc.lib ];
  RUSTFLAGS = "-C link-arg=-fuse-ld=mold -C link-arg=-Wl,-rpath,${stdenv.cc.cc.lib}/lib";

  buildInputs = [
    zerostack-toolchain
  ];
}
