using Scenario2 = Scenario
    <IsTriviallyCopyConstructible::Not, IsTriviallyMoveConstructible::Not,
IsTriviallyCopyAssignable::Not,
    IsTriviallyMoveAssignable::Not, IsTriviallyDestructible::Yes>;
using Map = std::map<int,
    std::string>;
