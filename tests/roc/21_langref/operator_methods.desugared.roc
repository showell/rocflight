# Syntax: operators dispatch to methods — desugared, explicit types.
#
# Every operator in roc is spelled as a method on its left operand, so a type that
# defines the method gets the operator:
#
#     +  plus        /   div_by         <   is_lt     ==  is_eq
#     -  minus       //  div_trunc_by   >   is_gt     !=  is_eq, negated
#     *  times       %   rem_by         <=  is_lte    -x  negate
#                                       >=  is_gte
app [main!] {}

Money :: { cents: I64 }.{
    plus : Money, Money -> Money
    plus = |a, b| { cents: a.cents + b.cents }

    minus : Money, Money -> Money
    minus = |a, b| { cents: a.cents - b.cents }

    negate : Money -> Money
    negate = |a| { cents: 0 - a.cents }

    is_eq : Money, Money -> Bool
    is_eq = |a, b| a.cents == b.cents
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    a : Money
    a = Money.{ cents: 5 }
    b : Money
    b = Money.{ cents: 7 }
    sum : Money
    sum = a + b
    diff : Money
    diff = a - b
    neg : Money
    neg = -a
    echo!("${sum.cents.to_str()},${diff.cents.to_str()},${neg.cents.to_str()}")
    echo!("${Str.inspect(a == b)},${Str.inspect(a != b)},${Str.inspect(a == a)}")
    Ok({})
}
