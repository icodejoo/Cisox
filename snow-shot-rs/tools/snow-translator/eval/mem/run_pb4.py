exec(open('run_pb.py').read().split("J=[")[0])
T='tok_un6ccm.json'
J=[('un6_base_p',U,T,[]),('un6_ext_beam2','_exp/un6-ext',T,['--beams','2'])]
exec("want="+open('run_pb.py').read().split("want=")[1])
