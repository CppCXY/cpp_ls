namespace outer { namespace inner {
int value = 1;
namespace {
int hidden = 2;
}
} // namespace inner
} // namespace outer
namespace outer::cxx17 {
int modern = outer::inner::value + outer::inner::hidden;
}
using namespace outer::cxx17;
namespace alias = outer::inner;
int use() { return alias::value + modern; }
