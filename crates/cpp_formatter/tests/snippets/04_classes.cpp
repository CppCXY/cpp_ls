namespace ns {
class Widget {
public:
    Widget() : a_(0), b_(1) {}
    explicit Widget(int a) : a_(a), b_(a * 2) {}
    virtual ~Widget() = default;
    virtual void run() = 0;
    void stop();
protected:
    int a_;
private:
    int b_;
};
void Widget::stop() {
    if (a_) { b_ = 2; }
    while (b_ > 0) { --b_; }
    do { ++a_; } while (a_ < 3);
}
} // namespace ns
