struct Money {
    long cents;
    Money operator+(const Money& o) const { return {cents + o.cents}; }
    Money& operator+=(const Money& o) { cents += o.cents; return *this; }
    bool operator==(const Money& o) const { return cents == o.cents; }
    Money& operator[](int) { return *this; }
    explicit operator bool() const { return cents != 0; }
    Money* operator->() { return this; }
    Money& operator=(const Money&) = default;
    Money(const Money&) = default;
    ~Money() = default;
    int operator()(int a,int b) const { return a+b; }
    bool operator<(const Money& o) const { return cents<o.cents; }
};
