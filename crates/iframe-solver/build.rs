fn main() {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("cpp/Solver.cpp")
        .file("cpp/Physics.cpp")
        .file("cpp/Gauss.cpp")
        .file("cpp/BIG.cpp")
        .file("cpp/XOREngine.cpp")
        .file("cpp/AdderEngine.cpp")
        .file("cpp/TheoryOracle.cpp")
        .file("cpp/MarchCubing.cpp")
        .file("cpp/ParallelSolver.cpp")
        .file("cpp/cdcl_bridge.cpp")
        .include("cpp");

    if cfg!(target_env = "msvc") {
        build
            .flag("/O2")
            .flag("/fp:fast")
            .flag("/EHsc")
            .define("NDEBUG", None);
    } else {
        build
            .flag("-O3")
            .flag("-DNDEBUG");
    }

    build.compile("physics_cdcl");

    println!("cargo:rerun-if-changed=cpp/");
}
