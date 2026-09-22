/// @file real_world.cpp
/// @brief A realistic C++20 module unit, written the way a person writes one.
///
/// This file is a test fixture, not a sample: `tests/ast.rs` and `tests/doc.rs` parse it and assert
/// what comes out. It is deliberately written with the layout a real file has — blank-line-separated
/// sections, a namespace wrapping everything, and documentation comments in the positions people
/// actually write them — because per-construct tests cannot catch a regression in how constructs
/// interact.
module;

#include <vector>
#include <string>

export module shapes;

import <iostream>;
import :geometry;

namespace shapes {

/// The largest grid this module will allocate.
inline constexpr int kMax = 16;

/// Which primitive a shape is.
enum class Kind : unsigned char { Circle, Square, Triangle };

/// A point in the plane.
///
/// The coordinates are private and read through accessors, so the invariant that a point is always
/// finite has one place to live.
struct Point {
public:
    /// Constructs a point.
    /// @param x  the horizontal coordinate
    /// @param y  the vertical coordinate
    Point(double x, double y) : x_(x), y_(y) {}

    /// @returns the horizontal coordinate
    double x() const noexcept { return x_; }

    /// @returns the vertical coordinate
    double y() const noexcept { return y_; }

private:
    double x_ = 0.0;
    double y_ = 0.0;
};

/// A fixed-size grid.
///
/// @tparam T  the element type
/// @tparam N  how many elements fit, which is part of the type rather than the value
template <typename T, int N = 3>
class Grid {
public:
    /// @param i  the index, which must be less than `N`
    /// @returns a reference to the element
    /// @code
    /// Grid<double, 4> g;
    /// g.at(0) = 1.0;
    /// @endcode
    T& at(int i) { return cells_[i]; }

    /// @copydoc at(int)
    const T& at(int i) const { return cells_[i]; }

private:
    T cells_[N];
};

/// The interface every shape implements.
///
/// @note Deliberately has no data members: a shape's state belongs to the shape.
class Shape {
public:
    virtual ~Shape() = default;

    /// @returns the area, in the unit the shape was constructed with
    /// @warning Not constexpr: the calculation may not be usable at compile time.
    virtual double area() const = 0;

    auto name() -> const char* { return name_; }

protected:
    const char* name_ = "shape";
};

/// The squared distance between two points.
///
/// Squared, not Euclidean: the caller usually only compares distances, and the square root is the
/// expensive part.
///
/// @param a  the first point
/// @param b  the second point
/// @returns the squared distance
double distance(const Point& a, const Point& b) {
    double dx = a.x() - b.x();
    double dy = a.y() - b.y();
    return dx * dx + dy * dy;
}

}  // namespace shapes

export using Point = shapes::Point;
export int helper();
