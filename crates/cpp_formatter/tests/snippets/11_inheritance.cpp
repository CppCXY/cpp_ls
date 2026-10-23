struct Base { virtual ~Base() = default; virtual void f() = 0; };
struct Derived final : Base, private std::enable_shared_from_this<Derived> {
    void f() override;
    virtual void g() const noexcept final { }
};
void Derived::f() { }
struct Alias final : public Base { void f() override { } };
