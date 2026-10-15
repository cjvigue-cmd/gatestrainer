def shipping(weight_kg, express):
    if weight_kg <= 0:
        raise ValueError("weight must be positive")
    if weight_kg <= 1:
        cost = 5
    else:
        if weight_kg <= 5:
            cost = 5 + (weight_kg - 1) * 2
        else:
            cost = 13 + (weight_kg - 5) * 1.5
    if express == True:
        cost = cost * 2
    return round(cost, 2)
