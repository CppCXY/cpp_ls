enum class Kind { A, B, C };
int classify(Kind k) {
    switch (k) {
    case Kind::A:
        [[fallthrough]];
    case Kind::B:
        return 1;
    case Kind::C: {
        return 2;
    }
    default:
        break;
    }
    return 0;
}
