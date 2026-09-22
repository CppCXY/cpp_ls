module;
#include <vector>
#include <string>

export module shapes;

import <iostream>;
import :geometry;

namespace shapes {

inline constexpr int kMax = 16;

enum class Kind : unsigned char { Circle, Square, Triangle };

struct Point {
public:
    Point(double x, double y) : x_(x), y_(y) {}
    double x() const noexcept { return x_; }
    double y() const noexcept { return y_; }
private:
    double x_ = 0.0;
    double y_ = 0.0;
};

template <typename T, int N = 3>
class Grid {
public:
    T& at(int i) { return cells_[i]; }
    const T& at(int i) const { return cells_[i]; }
private:
    T cells_[N];
};

class Shape {
public:
    virtual ~Shape() = default;
    virtual double area() const = 0;
    auto name() -> const char* { return name_; }
protected:
    const char* name_ = "shape";
};

double distance(const Point& a, const Point& b) {
    double dx = a.x() - b.x();
    double dy = a.y() - b.y();
    return dx * dx + dy * dy;
}

}  // namespace shapes

export using Point = shapes::Point;
export int helper();
