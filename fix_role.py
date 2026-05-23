with open('src/bots.rs', 'r') as f:
    lines = f.readlines()

for i, line in enumerate(lines):
    if "let role_color" in line and "bot.companion" in line:
        lines[i] = "    let role_color = bot_role_color(bot.role);\n"

with open('src/bots.rs', 'w') as f:
    f.writelines(lines)
