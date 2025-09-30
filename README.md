<p align="center">
  <img src="docs/images/logo/ascii-art-boon.png" alt="boon logo" style="max-width: 380px; width: 100%; border-radius: 24px; border: 1px solid #111629; box-shadow: 0 18px 36px rgba(0, 0, 0, 0.35);" />
</p>

<p align="center"><strong>Timeless playful programming language.</strong></p>

> noob or hacker, web or chip,  
boon will guide your coding trip

## First Look

**1**...  **2**...  **3**...  every second  
![interval code](docs/images/snippets/interval.png)

**1**...  **2**...  **3**...  on a button click
![counter code](docs/images/snippets/counter.png)

You can try out the examples on [play.boon.run](https://play.boon.run/)

## Super Counter

- Do you like open source?
- Do you like small computers like Raspberry Pi?
- Do you like hardware or FPGA boards like iCESugar? 

![raspberry pi and icesugar pro](docs/images/photos/raspbery_pi_and_icesugar.jpg)

- Would you like to program and configure everything in one language?
- Would you like to unplug it whenever you want to without losing data?

![pi and sugar diagram](docs/images/diagrams/pi_and_sugar.png)

What's on that diagram? Super counter!

1. You press a button on the iCESugar dev board.
2. The board sends a message to the Raspberry Pi.
3. The computer increments the counter in its database.
4. The board, the CLI (_command-line interface_), and the web app are notified that the counter value has changed.
5. The board turns on the LED for a moment to signal that the counter has been incremented.
6. The web app updates the rendered counter value and changes its color according to the defined script rules (e.g. green when the counter is less than 10, otherwise red).
7. The CLI displays the new counter value and runs a configured command (e.g. show a notification or write something into the terminal/console).
8. You can watch how data flows through the entire Super counter on the Monitor.

## Flow Combinators

### LATEST

![LATEST code example](docs/images/snippets/latest.png)

- The first value flying in is the first one flying out.
- When it's not possible to determine which value arrived first, the order in the LATEST block matters: `LATEST { first, second }`.
- The LATEST combinator also works across code changes. That means when you run the code `LATEST { 1, 2 }`, change the code to `LATEST { 3, 2 }`, and run the program again, the values coming out of that combinator will be: `1`, then `2`, and then `3`. Number `2` hasn't changed, so it doesn't fly out on the second run.

![LATEST diagram](docs/images/diagrams/latest.png)

- **EXTRA:** `Math/sum(increment<Number>) -> Number` and other function calls, operators and combinators are actually _actors_. It means a stream of data flows inside and outside of them and they keep their internal state. In the case of `Math/sum`, it remembers the current sum and outputs it on every change, that is, on every new incoming value.

![Math/sum diagram](docs/images/diagrams/math_sum.png)

### WHILE

![WHILE code example](docs/images/snippets/while.png)

- Let's say you have sensors counting cars on two roads:
    1. `input_a` is the changing sum of the cars passed on road `A`
    2. `input_b` is the changing sum of the cars passed on road `B`
- Both inputs are connected to your fancy computer, where you can switch between their:
    1. `Addition` - to know how many cars were on both roads
    2. `Subtraction` - to know which road is more used
- That means when `input_a` is `15` and `input_b` is `7`, you can see either `22` or `8`, and the displayed value changes in real time with every car that passes by.  

![WHILE diagram](docs/images/diagrams/while.png)

### WHEN

![WHEN code example](docs/images/snippets/when.png)

- Bob doesn't have such a fancy computer. Bob has a paper and a pen.
- Every time he wants to record `Addition` or `Subtraction`, he has to write the result down.
- That means he loses input values between entries, but the written results never change.
- Everything inside the `WHEN { ... }` block is "frozen in time" when an _input value_ arrives (`Sub` or `Add` on the diagram below, `operation` in the code above) - values are copied, and they no longer change according to their _dependencies_ (`A` and `B` in the diagram, `input_a` and `input_b` in the code). 

![WHEN diagram](docs/images/diagrams/when.png)

### THEN

![THEN code example](docs/images/snippets/then.png)

- Alice only has paper and a pen like Bob, but she writes down only additions.
- `THEN` is basically `WHEN` without _arms_ (`Abc => 123`). You can replace `THEN { 123 }` with `WHEN { __  => 123 }`; that means you don't care about the actual input value, you just want to do something when a new input arrives. 

![THEN diagram](docs/images/diagrams/then.png)

### Summary

- Data flow continuously **WH**ILE an arm is selected.
- Data are copied **WH***EN* an arm is selected.
- Input arrives, and TH*EN* data are copied.

### Real-world examples 
1. When the user presses Enter or clicks a send button in your chat application, use WHEN or THEN to copy the message written in the new-message input. With the WHILE combinator, you'd risk changing already sent messages with every change to the text input.
2. Use WHILE to update texts in your multilingual application. With WHEN/THEN you'd update all dynamic texts only when the user decides to switch languages.

## Durable State & Code Changes

Every program is a living organism, sooner or later you'll want to change its code but ideally not lose any data during the process or tell your users to not use your app during the weekend while you are "migrating data".

Let's say we want to upgrade our simple counter example - rename the variable `counter` to `counter_2` and increment its value by 2 on a button click.

It would be a pretty straightforward operation, but we don't want to reset our counter value while deploying a new app version.

You can go to [play.boon.run](https://play.boon.run/), click the button `counter.bn` in the header and follow the steps below with me.

1. Look at the original counter code, run it and press the `+` button to change counter state a bit.
![counter upgrade step 1](docs/images/snippets/state_migrations/counter_1.png)
2. Add the `counter_2` definition, replace `counter` with `counter_2` in HTML document items, and prevent the old `counter` from listening to button-press events. Also include the old `counter` in `counter_2`'s LATEST blocks to do the actual state migration from the old counter to the new one. Then run the example again. Nothing should change visually, but you're already using the upgraded app.
![counter upgrade step 2](docs/images/snippets/state_migrations/counter_2.png)
3. Remove the references to the old `counter`, delete its definition, and we are done!
![counter upgrade step 3](docs/images/snippets/state_migrations/counter_3.png)
4. When you want to reset the counter, click the **Clear saved states** button just above the preview pane on the playground to remove all states stored in the browser's LocalStorage, and then click the **Run** button to restart the app; the counter resets back to 0.

**EXTRA:** Variables in our playground examples are stored in the browser. However, the general idea is that some variables in your app will be stored in the browser, some variables in a standard database, and some of them nowhere to save memory or quickly forget things like passwords or tokens.

## All roads lead to ~~Rome~~ `document`

![counter flow diagram](docs/images/diagrams/counter_flow.png)

Look at that counter example dataflow diagram again. Do all paths really lead to the `document`? Almost! The only exception is that blue `LINK` rectangle and the bottom left corner coming from the `Element/button(..)` function call. 

What is that `LINK` good for? Why does it make the only loop in the entire diagram to ruin my otherwise perfect tree?!

Look at the counter code again:

![counter code](docs/images/snippets/counter.png)

1. Notice the `press: LINK` object field being passed to the `Element/button` function call as part of the `element` argument.
2. Function `Element/button` transforms the arguments into a data structure compatible with the element tree and digestible by the `Document/new` function.
3. Boon browser runtime finds the global `document` variable and creates browser elements described in the passed element tree. When it finds `event.press`, it _links_ it with events produced by those browser HTML/DOM elements.
4. When linked, you can listen for new events with code like `my_button.event.press |> THEN { .. }`

So, for our current understanding, `variable_name: LINK` basically means that the variable's value can be set **after** the variable is defined - no other variables can be set once they are defined.

## Finally, perfect trees!

We cannot really get rid of all loops in dataflow graphs - they are a natural part of programs and the things we do - we can only monitor them and try to remove accidental infinite loops.

However, look at this nice forest!:

![counter state diagram](docs/images/diagrams/counter_state.png)

- In Boon, every piece of state has a place in the _ownership hierarchy_.
Detach an _object_ or a _list_, and all of its descendants vanish with it.
Nothing is “freed” manually — the runtime simply drops what’s no longer owned, whether data or actors.
- Global variables - `document`, `counter`, `increment_button` - own all other items in our counter example. Everything has only one owner: the place where it was defined; everything else is just a _reference_ marked with dashed arrows.
- That's why `LINK` is actually a link/reference - the variable defined with `LINK` doesn't _own_ the linked browser element (notice the blue dashed OBJECT at the bottom of the state diagram above).

## Enough diagrams! More code!


![complex counter example](docs/images/snippets/complex_counter.png)

### PASS + PASSED

Notice in the code above: 

1. `root_element(PASS: store)`
2. `FUNCTION root_element()`  
3. `PASSED.elements.increment_button`

The only purpose is to _pass_ data through multiple function calls without the need to mention them explicitly among function arguments.

Without PASS + PASSED, the same lines would look like:
1. `root_element(store: store)` or `store |> root_element()`
2. `FUNCTION root_element(store)`   <- new function argument
3. `store.elements.increment_button`

So PASS + PASSED is useful when you have deep function call tree (typically element tree) and bottom levels need something from top levels.

### LINK { .. }

You already know what `variable: LINK` means and now you'll find out how to set it by yourself (instead of setting it by Boon runtime). 

Notice these lines: 

1. `decrement_button: LINK`
2. `counter_button(label: '-') |> LINK { PASSED.elements.decrement_button }`

The element data returned from the `counter_button` function call are _linked_ to `decrement_button` and returned from `LINK {}` without any changes. 

## Where is Fibonacci??

![fibonacci example](docs/images/snippets/fibonacci.png)

This idea was driven by design decisions to avoid recursions and keep loops as tight and hidden as possible. However, I want the Boon design to be driven by real applications, so I'll revisit this API/example when the need for channels, (infinite) streams, generators, lazy evaluation, tail recursions, or other related concepts emerges.

## See the Problem, Fix the Flow

1. A monitor/debugger is your friend. Have you ever played a Factorio-like game? I want to:
    - SEE the problem.
    - See statistics.
    - Be able to immediately fix the problem.
    - Want to know why the problem happened.
    - Want to see slow parts.
    - Want to see loops.
    - Want to see what is waiting and why.
    - Want to just watch and enjoy it while everything works as expected.
    - Want to be notified when something fails. 
Just show me! Yes, you understand correctly, monitoring and a short feedback loop have high priority for Boon tools and design in general. 

2. A compiler is your friend. Nice error messages, fast type checking, hints. 

![error example](docs/images/snippets/errors/unknown_function_in_browser.png)

3. A formatter is your friend. No need to think about the correct number of spaces. No confusing code diffs. Constant reading speed for a code reviewer.

## Status & Future

A lot of things have to be implemented and explained, but the core is there and I don't plan to stop!

Examples on [play.boon.run](https://play.boon.run/) are guaranteed to run. I'll continuously add more there, and examples running outside the browser environment will appear as well.

Questions ▷ martin@kavik.cz

## Credits

- ASCII Art: [patorjk.com + Mono 12](https://patorjk.com/software/taag/#p=display&f=Mono+12&t=boon&x=rainbow3&v=4&h=4&w=80&we=false)
