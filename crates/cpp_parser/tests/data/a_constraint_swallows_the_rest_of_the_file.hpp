// A reproduction of the parse that swallows the rest of the file: the `namespace probe` at the bottom is parsed
// as `std::probe`, i.e. one scope is left open by the class above.
//
// Reduced from MSVC's `<ranges>` (the `lazy_split_view` declaration, offsets 108 422-118 716 of the cooked
// rendering's body). The class **body is not needed** — the head plus the two constructors is the whole of it — and
// the trigger is the second constructor's constraint: with only its first conjunct
// (`constructible_from<_Vw, views::all_t<_Rng>>`) the bug still happens, with only the second it does not, and
// renaming the concept changes nothing. See `docs/ranges.md` for the reduction table.
namespace std {
namespace ranges {
template <input_range _Vw, forward_range _Pat>
    requires(view<_Vw> && view<_Pat> && indirectly_comparable<iterator_t<_Vw>, iterator_t<_Pat>, ::std::ranges::equal_to> && (forward_range<_Vw> || _Tiny_range<_Pat>))
class lazy_split_view : public _Lazy_split_view_base<_Vw, _Pat> {
    constexpr lazy_split_view(_Vw _Range_, _Pat _Pattern_) noexcept(is_nothrow_move_constructible_v<_Vw> && is_nothrow_move_constructible_v<_Pat>)
        : _Range(::std::move(_Range_)), _Pattern(::std::move(_Pattern_)) {}

    template <input_range _Rng>
        requires constructible_from<_Vw, views::all_t<_Rng>>
    constexpr lazy_split_view(_Rng&& _Range_, range_value_t<_Rng> _Elem)
        noexcept(noexcept(_Vw(views::all(::std::forward<_Rng>(_Range_)))) && noexcept(_Pat(views::single(::std::move(_Elem)))))
        : _Range(views::all(::std::forward<_Rng>(_Range_))), _Pattern(views::single(::std::move(_Elem))) {}
}; // class lazy_split_view
} // namespace ranges
} // namespace std

namespace probe {
int marker;
}
